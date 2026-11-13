//! Golden and property tests of the card renderer (native).
//!
//! - Every card fixture of `detect-page` (variant x state) renders to the
//!   PNG digest recorded in `tests/golden/digests.txt`, with the icon ends
//!   and, for a subset, the letter ends. `UPDATE_GOLDEN=1` rewrites the
//!   file; `RENDER_DIR=<dir>` also writes each PNG there for a look.
//! - Secret cards: two secret pages that differ only in slug give equal
//!   cards and equal PNG bytes, for every band, the short results, the free
//!   cut and the paid document; no secret card's JSON, PNG chunk or download
//!   name holds a slug, and its link prints only the secret host.
//! - Hostile strings (XML specials, bidi and invisible controls, characters
//!   outside the fonts, the longest host, a long origin): escaped into text
//!   nodes, refused, or fitted, never breaking the document.
//! - The SVG holds no external reference, no image, script or foreign
//!   object, and unique ids; card strings appear only as text.
//! - The layout's metrics agree with the shaper's advances.

use resvg::usvg;
use sha2::{Digest, Sha256};
use slop_ninja_detect_og::card::{self, CardRefusal};
use slop_ninja_detect_og::fonts::{Face, Weight};
use slop_ninja_detect_og::render::{self, RenderError};
use slop_ninja_detect_og::wire::{self, RenderRequest};
use slop_ninja_detect_page::page::Page;
use slop_ninja_detect_page::{Band, Card, CardLink, GaugeEnds, Origins};
use std::collections::BTreeMap;
use std::path::PathBuf;

fn origins() -> Origins {
    Origins::from_vars("https://slop.ninja", "https://s.slop.ninja").unwrap()
}

fn page_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../crates/detect-page/tests/fixtures")
}

fn cards() -> Vec<(String, Card)> {
    let mut out = Vec::new();
    for e in std::fs::read_dir(page_dir().join("cards")).unwrap() {
        let p = e.unwrap().path();
        let name = p.file_stem().unwrap().to_string_lossy().into_owned();
        let card: Card = serde_json::from_str(&std::fs::read_to_string(&p).unwrap()).unwrap();
        out.push((name, card));
    }
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out
}

fn page(name: &str) -> Page {
    serde_json::from_str(&std::fs::read_to_string(page_dir().join(format!("pages/{name}.json"))).unwrap()).unwrap()
}

fn digest(b: &[u8]) -> String {
    Sha256::digest(b).iter().map(|x| format!("{x:02x}")).collect()
}

fn png(card: &Card, ends: GaugeEnds) -> Vec<u8> {
    render::card_png(card, &origins(), ends).unwrap_or_else(|e| panic!("{e:?}"))
}

/// The PNG's chunk types, in order.
fn chunks(png: &[u8]) -> Vec<String> {
    assert_eq!(&png[..8], b"\x89PNG\r\n\x1a\n");
    let mut out = Vec::new();
    let mut p = 8;
    while p + 8 <= png.len() {
        let len = u32::from_be_bytes(png[p..p + 4].try_into().unwrap()) as usize;
        out.push(String::from_utf8_lossy(&png[p + 4..p + 8]).into_owned());
        p += 12 + len;
    }
    assert_eq!(p, png.len());
    out
}

fn golden_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/golden/digests.txt")
}

/// Every golden case: fixture cards with the icon ends, and the letter ends
/// for one card of each variant.
fn golden_cases() -> Vec<(String, Card, GaugeEnds)> {
    let mut v = Vec::new();
    for (name, card) in cards() {
        v.push((name.clone(), card.clone(), GaugeEnds::Icons));
        if ["band-very_strong-strike", "short-strong-bluesky", "paid-document", "generic-weak"].contains(&name.as_str())
        {
            v.push((format!("{name}.letters"), card, GaugeEnds::Letters));
        }
    }
    v
}

#[test]
fn every_card_matches_its_golden_digest() {
    let mut got = BTreeMap::new();
    let dir = std::env::var_os("RENDER_DIR").map(PathBuf::from);
    for (name, card, ends) in golden_cases() {
        let bytes = png(&card, ends);
        let kinds = chunks(&bytes);
        assert_eq!(kinds.first().map(String::as_str), Some("IHDR"), "{name}");
        assert_eq!(kinds.last().map(String::as_str), Some("IEND"), "{name}");
        assert!(kinds[1..kinds.len() - 1].iter().all(|k| k == "IDAT"), "{name}: only image chunks: {kinds:?}");
        if let Some(d) = &dir {
            std::fs::write(d.join(format!("{name}.png")), &bytes).unwrap();
        }
        got.insert(name, digest(&bytes));
    }
    let text: String = got.iter().map(|(k, v)| format!("{v}  {k}\n")).collect();
    if std::env::var_os("UPDATE_GOLDEN").is_some() {
        std::fs::write(golden_path(), &text).unwrap();
        return;
    }
    let want = std::fs::read_to_string(golden_path()).expect("tests/golden/digests.txt (UPDATE_GOLDEN=1 writes it)");
    if want != text {
        let w: BTreeMap<&str, &str> = want.lines().filter_map(|l| l.split_once("  ").map(|(d, n)| (n, d))).collect();
        let diff: Vec<String> = got
            .iter()
            .filter(|(k, v)| w.get(k.as_str()) != Some(&v.as_str()))
            .map(|(k, _)| k.clone())
            .chain(w.keys().filter(|k| !got.contains_key(**k)).map(|k| format!("{k} (gone)")))
            .collect();
        panic!("golden digests differ for {diff:?}; rerun with UPDATE_GOLDEN=1 after checking the renders");
    }
}

#[test]
fn every_fixture_variant_and_state_is_covered() {
    use slop_ninja_detect_page::{CardVariant, NinjaState};
    let all = cards();
    for v in [CardVariant::Band, CardVariant::Short, CardVariant::PaidDocument, CardVariant::Generic] {
        assert!(all.iter().any(|(_, c)| c.variant == v), "{v:?}");
    }
    for s in [NinjaState::Bow, NinjaState::Stoic, NinjaState::Ready, NinjaState::Strike] {
        assert!(all.iter().any(|(_, c)| c.ninja == Some(s)), "{s:?}");
    }
    assert!(all.iter().any(|(_, c)| c.source_label == slop_ninja_detect_page::card::X_LABEL), "a post on X");
}

/// Secret pages that differ only in slug: equal cards, equal bytes.
#[test]
fn secret_cards_that_differ_only_in_slug_render_the_same_bytes() {
    let other = |s: &str| if s == "c3jf8kq1zw5ty7hn2mdx" { "7hq2kd9m4xw8rt3vn6pb" } else { "c3jf8kq1zw5ty7hn2mdx" };
    let mut pages: Vec<(String, Page)> = Vec::new();
    for name in
        ["15-secret-text", "16-secret-short-strong", "17-secret-short-none", "20-secret-free-cut", "24-paid-document"]
    {
        pages.push((name.into(), page(name)));
    }
    // Every band on the secret text page, at a share inside each band.
    let base = page("15-secret-text");
    for (b, share) in [(Band::Weak, 12u8), (Band::Unclear, 63), (Band::Strong, 41), (Band::VeryStrong, 86)] {
        let mut p = base.clone();
        let run = p.run.as_mut().unwrap();
        run.band = Some(b);
        run.share_pct = Some(share);
        p.ninja = slop_ninja_detect_page::ninja_state(
            slop_ninja_detect_page::NinjaInput::Banded { band: b, share_pct: share },
            false,
        );
        pages.push((format!("15-secret-text/{}", b.as_str()), p));
    }
    for (name, a) in pages {
        let slug = a.slug.clone().unwrap();
        let mut b = a.clone();
        b.slug = Some(other(&slug).into());
        for generic in [false, true] {
            let (ca, cb) = (Card::from_page(&a, generic).unwrap(), Card::from_page(&b, generic).unwrap());
            assert_eq!(ca, cb, "{name}");
            assert_eq!(ca.link, CardLink::SecretHost, "{name}");
            let (pa, pb) = (png(&ca, GaugeEnds::Icons), png(&cb, GaugeEnds::Icons));
            assert_eq!(digest(&pa), digest(&pb), "{name}: the slug changed the PNG");
            let json = serde_json::to_string(&ca).unwrap();
            let svg = card::svg(&ca, &origins(), GaugeEnds::Icons).unwrap();
            for s in [slug.as_str(), other(&slug)] {
                assert!(!json.contains(s), "{name}: slug in card JSON");
                assert!(!svg.contains(s), "{name}: slug in the SVG");
                assert!(!pa.windows(s.len()).any(|w| w == s.as_bytes()), "{name}: slug in the PNG");
                assert!(!wire::DOWNLOAD_NAME.contains(s));
            }
            assert!(svg.contains(">s.slop.ninja<"), "{name}: the secret host is printed");
            assert!(!svg.contains("s.slop.ninja/"), "{name}: nothing after the secret host");
        }
    }
    assert_eq!(wire::DOWNLOAD_NAME, "slopninja-report.png");
}

fn hostile_base() -> Card {
    cards().into_iter().find(|(n, _)| n == "band-very_strong-strike").unwrap().1
}

#[test]
fn xml_specials_stay_text() {
    let mut c = hostile_base();
    c.source_label = r#"</text><image href="https://x.example/a.png"/>&amp;'"<!--"#.into();
    c.model_line = "]]><![CDATA[ & <script>".into();
    let svg = card::svg(&c, &origins(), GaugeEnds::Icons).unwrap();
    let doc = roxmltree::Document::parse(&svg).unwrap();
    assert!(doc.descendants().all(|n| n.tag_name().name() != "image" && n.tag_name().name() != "script"));
    let texts: String = doc.descendants().filter(|n| n.is_text()).map(|n| n.text().unwrap()).collect();
    assert!(texts.contains("]]><![CDATA[ & <script>") || texts.contains('\u{2026}'), "the model line is text");
    assert!(texts.contains("&amp;'\"<!--") || texts.contains('\u{2026}'), "the label is text");
    let bytes = png(&c, GaugeEnds::Icons);
    assert_eq!(&bytes[..4], b"\x89PNG");
}

#[test]
fn bidi_and_invisible_controls_are_refused() {
    for bad in ['\u{202E}', '\u{2066}', '\u{200B}', '\u{FEFF}', '\u{0000}', '\u{001B}', '\u{00AD}'] {
        for field in 0..3 {
            let mut c = hostile_base();
            let s = format!("garden{bad}diary.example");
            match field {
                0 => c.source_label = s,
                1 => c.model_line = s,
                _ => c.headline = format!("Very strong{bad} evidence of AI involvement."),
            }
            assert!(
                matches!(
                    render::card_png(&c, &origins(), GaugeEnds::Icons),
                    Err(RenderError::Card(CardRefusal::Invalid(_)))
                ),
                "{bad:?} in field {field}"
            );
        }
    }
}

#[test]
fn characters_outside_the_fonts_are_refused() {
    for s in ["café.example", "例え.example", "a\u{00A0}b", "x\u{2014}y"] {
        let mut c = hostile_base();
        c.source_label = s.into();
        assert_eq!(
            render::card_png(&c, &origins(), GaugeEnds::Icons),
            Err(RenderError::Card(CardRefusal::Unsupported)),
            "{s}"
        );
    }
    assert_eq!(
        render::card_png(&hostile_base(), &origins(), GaugeEnds::Emoji),
        Err(RenderError::Card(CardRefusal::Ends))
    );
}

#[test]
fn the_longest_host_and_a_long_origin_fit() {
    let label = |n: usize, ch: char| std::iter::repeat_n(ch, n).collect::<String>();
    let host = format!("{}.{}.{}.{}", label(63, 'a'), label(63, 'b'), label(63, 'c'), label(61, 'd'));
    assert_eq!(host.len(), 253);
    let mut c = hostile_base();
    c.source_label = host.clone();
    let svg = card::svg(&c, &origins(), GaugeEnds::Icons).unwrap();
    assert!(svg.contains('\u{2026}'), "a 253-octet host is cut in the middle");
    assert!(!svg.contains(&host));
    png(&c, GaugeEnds::Icons);
    let long = Origins::from_vars(&format!("https://{}.example", label(60, 's')), "https://s.slop.ninja").unwrap();
    let svg = card::svg(&c, &long, GaugeEnds::Icons).unwrap();
    roxmltree::Document::parse(&svg).unwrap();
    render::png(&svg).unwrap();
}

#[test]
fn the_document_is_self_contained() {
    for (name, c, ends) in golden_cases() {
        let svg = card::svg(&c, &origins(), ends).unwrap();
        let doc = roxmltree::Document::parse(&svg).unwrap_or_else(|e| panic!("{name}: {e}"));
        let mut ids = std::collections::BTreeSet::new();
        for n in doc.descendants().filter(|n| n.is_element()) {
            let tag = n.tag_name().name();
            assert!(!["image", "script", "foreignObject", "use", "a", "iframe"].contains(&tag), "{name}: <{tag}>");
            for a in n.attributes() {
                let v = a.value();
                assert!(a.name() != "href", "{name}: an href");
                assert!(!v.contains("http") && !v.contains("data:"), "{name}: {}={v}", a.name());
                if a.name() == "id" {
                    assert!(ids.insert(v.to_owned()), "{name}: duplicate id {v}");
                }
                // No card string becomes an attribute.
                for s in [&c.source_label, &c.headline, &c.model_line] {
                    assert!(!v.contains(s.as_str()), "{name}: a card string in {}", a.name());
                }
            }
        }
    }
}

#[test]
fn a_request_is_one_card_and_nothing_else() {
    let c = hostile_base();
    let ok = serde_json::json!({ "card": c });
    let _: RenderRequest = serde_json::from_value(ok).unwrap();
    let extra = serde_json::json!({ "card": c, "slug": "k7m2xq9" });
    assert!(serde_json::from_value::<RenderRequest>(extra).is_err());
    assert!(serde_json::from_str::<RenderRequest>("{}").is_err());
}

/// The layout measures with the subset's advances and kern table; the
/// shaper (harfrust, through usvg) must place the same advances, or lines
/// would be broken at the wrong width. An end-anchored copy of a text sits
/// one advance left of a start-anchored one.
#[test]
fn layout_metrics_match_the_shaper() {
    let opt = render::options();
    for (weight, size, spacing, s) in [
        (Weight::Bold, 42.0f32, -2.1f32, "Very strong evidence of AI involvement."),
        (Weight::Bold, 17.0, 1.7, "DETECTOR REPORT"),
        (Weight::Regular, 26.0, -0.26, "a page on garden-diary.example"),
        (Weight::Regular, 22.0, 0.0, "span package d24 \u{00B7} e2536d12 \u{00B7} publication pending"),
        (Weight::Bold, 30.0, -0.9, "AVATAR Toy Way. y. \u{2026}"),
    ] {
        let left = |anchor: &str| {
            let svg = format!(
                r#"<svg xmlns="http://www.w3.org/2000/svg" width="2000" height="100"><text x="1000" y="60" font-family="Inter" font-size="{size}" font-weight="{}" letter-spacing="{spacing}" text-anchor="{anchor}">{s}</text></svg>"#,
                weight.css()
            );
            let tree = usvg::Tree::from_str(&svg, &opt).unwrap();
            tree.root().abs_bounding_box().unwrap().left()
        };
        let shaped = left("start") - left("end");
        let ours = Face::of(weight).unwrap().width(s, size, spacing);
        assert!((shaped - ours).abs() <= 1.0 + spacing.abs(), "{s:?}: shaper {shaped}, layout {ours}");
    }
}
