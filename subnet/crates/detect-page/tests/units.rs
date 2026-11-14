//! The `detect-page` unit tests: slug drawing (uniform 5-bit symbols,
//! lengths, blocklist redraw), folding and the 301 forms; the ninja state
//! over every band and share edge, the mockup's own edge pairs, short
//! results, paid documents and generic cards; the card JSON's bounds and its
//! slug-free secret form; and the header, origin, refusal and switch
//! contracts.

use std::collections::HashSet;

use slop_ninja_detect_page::card::{self, Card, CardLink};
use slop_ninja_detect_page::ninja::{allowed, share_state};
use slop_ninja_detect_page::page::*;
use slop_ninja_detect_page::slug::{self, Folded, PathMatch, SlugRoute};
use slop_ninja_detect_page::sn::{self, InternalRoute, Principal, RouteId};
use slop_ninja_detect_page::*;

/// The origins every test uses (reserved names; no origin is compiled in).
fn test_origins() -> Origins {
    Origins::from_vars("https://site.example", "https://s.site.example").unwrap()
}

/// A deterministic byte stream (xorshift64*), for tests only.
struct Xs(u64);

impl Xs {
    fn fill(&mut self, buf: &mut [u8]) {
        for b in buf {
            self.0 ^= self.0 >> 12;
            self.0 ^= self.0 << 25;
            self.0 ^= self.0 >> 27;
            *b = (self.0.wrapping_mul(0x2545_F491_4F6C_DD1D) >> 56) as u8;
        }
    }
}

// ---------------------------------------------------------------- slugs

#[test]
fn draws_take_five_bits_per_symbol_msb_first() {
    let mut zeros = |b: &mut [u8]| b.fill(0);
    assert_eq!(slug::draw(7, &mut zeros), "0000000");
    let mut ones = |b: &mut [u8]| b.fill(0xff);
    assert_eq!(slug::draw(7, &mut ones), "zzzzzzz");
    // 0x08 0x86 0x42 0x98 0xe8 = 00001 00010 00011 00100 00101 00110 00111 01000
    let mut fixed = |b: &mut [u8]| b.copy_from_slice(&[0x08, 0x86, 0x42, 0x98, 0xe8][..b.len()]);
    assert_eq!(slug::draw(8, &mut fixed), "12345678");
    // Exactly ceil(len × 5 / 8) bytes are read.
    for (len, bytes) in [(7, 5), (20, 13), (26, 17)] {
        let mut asked = 0;
        let mut count = |b: &mut [u8]| {
            asked = b.len();
            b.fill(0)
        };
        assert_eq!(slug::draw(len, &mut count).len(), len);
        assert_eq!(asked, bytes);
    }
}

#[test]
fn symbols_are_uniform() {
    let mut x = Xs(0x9e37_79b9_7f4a_7c15);
    let mut counts = [0u32; 32];
    for _ in 0..4_000 {
        for c in slug::draw(26, &mut |b| x.fill(b)).bytes() {
            counts[slug::value(c).unwrap() as usize] += 1;
        }
    }
    // 104,000 symbols: 3,250 expected per symbol (sigma about 56); 10% is
    // nearly 6 sigma.
    for (v, n) in counts.iter().enumerate() {
        assert!((2_925..=3_575).contains(n), "symbol {v}: {n}");
    }
}

#[test]
fn lengths_and_alphabet() {
    let mut x = Xs(7);
    assert_eq!(slug::draw_secret(&mut |b| x.fill(b)).len(), slug::SECRET_LEN);
    assert_eq!(slug::draw_token(&mut |b| x.fill(b)).len(), slug::TOKEN_LEN);
    let p = slug::draw_public(&mut |b| x.fill(b)).unwrap();
    assert_eq!(p.len(), slug::PUBLIC_LEN);
    assert!(p.bytes().all(|c| slug::value(c).is_some()));
    for c in *b"ilouIA" {
        assert_eq!(slug::value(c), None);
    }
    assert_eq!(slug::ALPHABET.iter().collect::<HashSet<_>>().len(), 32);
}

#[test]
fn blocked_public_draws_are_redrawn() {
    for w in slug::BLOCKLIST_V1 {
        assert!(w.len() >= 3 && w.len() <= slug::PUBLIC_LEN, "{w}");
        assert!(w.bytes().all(|c| slug::value(c).is_some()), "{w} is not canonical");
    }
    // "sex" at the start: 11001 01110 11101 → the first draw is blocked.
    let blocked = [0xcb, 0xba, 0x00, 0x00, 0x00];
    let mut calls = 0;
    let mut src = |b: &mut [u8]| {
        calls += 1;
        if calls == 1 { b.copy_from_slice(&blocked) } else { b.fill(0x42) }
    };
    let first = slug::draw(7, &mut |b| b.copy_from_slice(&blocked));
    assert!(first.starts_with("sex") && slug::is_blocked(&first), "{first}");
    let got = slug::draw_public(&mut src).unwrap();
    assert!(!slug::is_blocked(&got));
    assert_eq!(calls, 2);
    // A source that only ever spells a blocked word is reported broken.
    assert_eq!(slug::draw_public(&mut |b| b.copy_from_slice(&blocked)), Err(slug::BrokenRandom));
    // A uniform draw is blocked well under 1% of the time.
    let mut x = Xs(11);
    let blocked = (0..20_000).filter(|_| slug::is_blocked(&slug::draw(7, &mut |b| x.fill(b)))).count();
    assert!(blocked < 200, "{blocked} of 20,000");
}

#[test]
fn folding() {
    assert_eq!(slug::fold("k7m2xq9", 7), Folded::Canonical);
    assert_eq!(slug::fold("K7M2XQ9", 7), Folded::Fold("k7m2xq9".into()));
    assert_eq!(slug::fold("kIlOoL9", 7), Folded::Fold("k110019".into()));
    assert_eq!(slug::fold("k7m2xq", 7), Folded::Invalid);
    assert_eq!(slug::fold("k7m2xq9z", 7), Folded::Invalid);
    assert_eq!(slug::fold("k7m2xqu", 7), Folded::Invalid);
    assert_eq!(slug::fold("k7m2xq-", 7), Folded::Invalid);
    assert_eq!(slug::fold("k7m2x\u{e9}", 7), Folded::Invalid);
    assert!(slug::is_canonical("7hq2kd9m4xw8rt3vn6pb", slug::SECRET_LEN));
}

#[test]
fn page_paths_and_their_301_forms() {
    use HostRole::{Secret, Site};
    let serve = |route, s: &str| PathMatch::Serve { route, slug: s.into() };
    let to = |p: &str| PathMatch::Redirect(p.into());
    assert_eq!(slug::match_path(Site, "/d/k7m2xq9"), serve(SlugRoute::Page, "k7m2xq9"));
    assert_eq!(slug::match_path(Site, "/d/k7m2xq9/card.png"), serve(SlugRoute::Card, "k7m2xq9"));
    assert_eq!(slug::match_path(Site, "/d/K7M2XQ9"), to("/d/k7m2xq9"));
    assert_eq!(slug::match_path(Site, "/d/K7M2XQ9/card.png"), to("/d/k7m2xq9/card.png"));
    assert_eq!(slug::match_path(Site, "/d/k7m2xq9/"), to("/d/k7m2xq9"));
    assert_eq!(slug::match_path(Site, "/d/KO1L2xq/"), to("/d/k0112xq"));
    assert_eq!(slug::match_path(Site, "/d/k7m2xq9/manage"), PathMatch::NoMatch);
    assert_eq!(slug::match_path(Site, "/d/k7m2xq"), PathMatch::NoMatch);
    assert_eq!(slug::match_path(Site, "/d/7hq2kd9m4xw8rt3vn6pb"), PathMatch::NoMatch);
    assert_eq!(slug::match_path(Site, "/k7m2xq9"), PathMatch::NoMatch);
    let s = "7hq2kd9m4xw8rt3vn6pb";
    assert_eq!(slug::match_path(Secret, &format!("/{s}")), serve(SlugRoute::Page, s));
    assert_eq!(slug::match_path(Secret, &format!("/{s}/manage")), serve(SlugRoute::Manage, s));
    assert_eq!(slug::match_path(Secret, &format!("/{s}/card.png")), serve(SlugRoute::Card, s));
    assert_eq!(slug::match_path(Secret, &format!("/{}", s.to_uppercase())), to(&format!("/{s}")));
    assert_eq!(slug::match_path(Secret, &format!("/{}/manage", s.to_uppercase())), to(&format!("/{s}/manage")));
    assert_eq!(slug::match_path(Secret, "/robots.txt"), PathMatch::NoMatch);
    assert_eq!(slug::match_path(Secret, "/d/k7m2xq9"), PathMatch::NoMatch);
    // X's widget route lives on the site host only.
    assert_eq!(slug::match_path(Site, "/d/k7m2xq9/x"), serve(SlugRoute::XWidget, "k7m2xq9"));
    assert_eq!(slug::match_path(Site, "/d/K7M2XQ9/x"), to("/d/k7m2xq9/x"));
    assert_eq!(slug::match_path(Site, "/d/k7m2xq9/x/"), PathMatch::NoMatch);
    assert_eq!(slug::match_path(Site, "/d/k7m2xq9/X"), PathMatch::NoMatch);
    assert_eq!(slug::path(Site, SlugRoute::XWidget, "k7m2xq9"), "/d/k7m2xq9/x");
    assert_eq!(slug::match_path(Secret, &format!("/{s}/x")), PathMatch::NoMatch);
}

// ---------------------------------------------------------------- ninja

#[test]
fn ninja_state_over_every_band_and_share_edge() {
    use NinjaState::*;
    let edges: [(u8, NinjaState); 8] =
        [(0, Bow), (24, Bow), (25, Stoic), (49, Stoic), (50, Ready), (74, Ready), (75, Strike), (100, Strike)];
    for (share, own) in edges {
        assert_eq!(share_state(share), own, "share {share}");
        for band in Band::ALL {
            let r = allowed(*band);
            assert!(r.start() <= r.end());
            let got = ninja_state(NinjaInput::Banded { band: *band, share_pct: share }, false).unwrap();
            assert!(r.contains(&got), "{band} {share} -> {got}");
            let want = match band {
                Band::Weak => own.min(Stoic),
                Band::Unclear => Stoic,
                Band::Strong => own.min(Ready),
                Band::VeryStrong => own.max(Stoic),
            };
            assert_eq!(got, want, "{band} {share}");
        }
    }
}

#[test]
fn ninja_state_matches_the_mockup_edges() {
    use NinjaState::*;
    // build.py's own asserts.
    let pairs: [(u8, Band, NinjaState); 11] = [
        (80, Band::Unclear, Stoic),
        (10, Band::Unclear, Stoic),
        (10, Band::VeryStrong, Stoic),
        (60, Band::VeryStrong, Ready),
        (75, Band::VeryStrong, Strike),
        (80, Band::Strong, Ready),
        (43, Band::Strong, Stoic),
        (4, Band::Strong, Bow),
        (30, Band::Weak, Stoic),
        (60, Band::Weak, Stoic),
        (4, Band::Weak, Bow),
    ];
    for (share, band, want) in pairs {
        assert_eq!(ninja_state(NinjaInput::Banded { band, share_pct: share }, false), Some(want), "{share} {band}");
    }
    assert_eq!(ninja_state(NinjaInput::Short(ShortResult::Strong), false), Some(Ready));
    assert_eq!(ninja_state(NinjaInput::Short(ShortResult::None), false), Some(Stoic));
    assert_eq!(ninja_state(NinjaInput::PaidDocument, false), None);
    for input in [NinjaInput::Banded { band: Band::VeryStrong, share_pct: 90 }, NinjaInput::Short(ShortResult::Strong)]
    {
        assert_eq!(ninja_state(input, true), None, "generic cards drop the state");
    }
}

#[test]
fn gauge_ends() {
    assert_eq!(GaugeEnds::default(), GaugeEnds::Icons);
    for e in GaugeEnds::ALL {
        assert_eq!(serde_json::to_string(e).unwrap(), format!("\"{}\"", e.as_str()));
        assert_eq!(GaugeEnds::parse(Some(e.as_str())), Some(*e));
    }
    assert_eq!(GaugeEnds::parse(None), Some(GaugeEnds::Icons));
    assert_eq!(GaugeEnds::parse(Some(" ")), Some(GaugeEnds::Icons));
    assert_eq!(GaugeEnds::parse(Some("Icons")), None);
    assert_eq!(GaugeEnds::LETTERS, ("H", "S"));
    assert!(GaugeEnds::HUMAN_ICON.ends_with("/gauge/human.svg") && GaugeEnds::SLOP_ICON.ends_with("/gauge/robot.svg"));
    assert!(GaugeEnds::HUMAN_ASSET.ends_with("/ninja.svg") && GaugeEnds::SLOP_ASSET.ends_with("/robot.svg"));
}

// ---------------------------------------------------------------- cards

fn fixture(name: &str) -> Page {
    let p = format!("{}/tests/fixtures/pages/{name}.json", env!("CARGO_MANIFEST_DIR"));
    serde_json::from_str(&std::fs::read_to_string(p).unwrap()).unwrap()
}

#[test]
fn secret_cards_never_carry_the_slug() {
    for name in ["15-secret-text", "16-secret-short-strong", "20-secret-free-cut", "24-paid-document"] {
        let a = fixture(name);
        let mut b = a.clone();
        b.slug = Some(
            if a.slug.as_deref() == Some("c3jf8kq1zw5ty7hn2mdx") {
                "7hq2kd9m4xw8rt3vn6pb"
            } else {
                "c3jf8kq1zw5ty7hn2mdx"
            }
            .into(),
        );
        let (ca, cb) = (Card::from_page(&a, false).unwrap(), Card::from_page(&b, false).unwrap());
        // Two secret pages that differ only in slug give the same card.
        assert_eq!(ca, cb, "{name}");
        assert_eq!(ca.link, CardLink::SecretHost);
        let json = serde_json::to_string(&ca).unwrap();
        for s in [a.slug.as_deref().unwrap(), b.slug.as_deref().unwrap()] {
            assert!(!json.contains(s), "{name}: slug in card JSON");
        }
        assert_eq!(ca.link.printed(&test_origins()), "s.site.example");
    }
    assert!(!card::DOWNLOAD_NAME.contains("k7m2xq9"));
    let public = Card::from_page(&fixture("01-public-very-strong"), false).unwrap();
    assert_eq!(public.link.printed(&test_origins()), "site.example/d/k7m2xq9");
}

#[test]
fn cards_follow_their_page() {
    assert_eq!(Card::from_page(&fixture("13-public-removed"), false), Err(card::NoCard::NotShown));
    assert_eq!(Card::from_page(&fixture("21-secret-deleted"), false), Err(card::NoCard::NotShown));
    assert_eq!(Card::from_page(&fixture("11-public-pending"), false), Err(card::NoCard::NotShown));
    assert_eq!(Card::from_page(&fixture("27-unstored"), false), Err(card::NoCard::Kind));
    assert_eq!(Card::from_page(&fixture("28-example"), false), Err(card::NoCard::Kind));
    let c = Card::from_page(&fixture("08-public-excerpt-withheld"), false).unwrap();
    assert_eq!((c.variant, c.ninja), (CardVariant::Generic, None));
    let c = Card::from_page(&fixture("01-public-very-strong"), true).unwrap();
    assert_eq!((c.variant, c.ninja), (CardVariant::Generic, None));
    let c = Card::from_page(&fixture("01-public-very-strong"), false).unwrap();
    assert_eq!((c.variant, c.ninja), (CardVariant::Band, Some(NinjaState::Strike)));
    assert_eq!(c.headline, "Very strong evidence of AI involvement.");
    assert_eq!(c.model_line, "span package d24 · e2536d12 · publication pending");
    let c = Card::from_page(&fixture("24-paid-document"), false).unwrap();
    assert_eq!(c.headline, "Strong signs of AI involvement in 1 of 3 sections.");
    assert_eq!(c.document_note.as_deref(), Some("Whole document, 3,214 words, 3 sections"));
    let p = fixture("20-secret-free-cut");
    let run = p.run.as_ref().unwrap();
    assert!(run.words <= 1000 && run.words < run.total_words);
    let c = Card::from_page(&p, false).unwrap();
    assert_eq!(c.free_cut_note, Some(card::free_cut_note(run.words, run.total_words)));
    assert_eq!(card::document_headline(0, 5), "No strong signs of AI involvement in any of the 5 sections.");
    // An X card says "a post on X" and nothing of the post, its host or its
    // author, at every length and however the post is shown.
    let c = Card::from_page(&fixture("29-x-post-short"), false).unwrap();
    assert_eq!(c.source_label, card::X_LABEL);
    assert_eq!(c.link.printed(&test_origins()), "site.example/d/x4m8q2t");
    assert!(c.check().is_ok());
    assert_eq!(Card::from_page(&fixture("33-x-post-gone"), false), Err(card::NoCard::NotShown));
    // A paste whose source is an X post is a secret card like any other.
    let c = Card::from_page(&fixture("19-secret-pasted-x-source"), false).unwrap();
    assert_eq!((c.source_label.as_str(), &c.link), (card::SECRET_LABEL, &CardLink::SecretHost));
}

#[test]
fn card_bounds_hold_for_extreme_values() {
    let max = u32::MAX;
    assert!(card::document_headline(max, max).len() <= card::MAX_HEADLINE);
    assert!(card::document_headline(0, max).len() <= card::MAX_HEADLINE);
    assert!(card::free_cut_note(max, max).len() <= card::MAX_NOTE);
    assert!(card::document_note(max, max).len() <= card::MAX_NOTE);
    let long_label = "x".repeat(200);
    assert!(card::model_line(&long_label, &"f".repeat(64), true).len() <= card::MAX_MODEL_LINE);
    for b in Band::ALL {
        assert!(card::band_headline(*b).len() <= card::MAX_HEADLINE);
    }
    for f in ["above 99.9%", "below 1%", "about 100%", "99.9%"] {
        assert!(f.len() <= card::MAX_FIGURE);
    }
    // A hostile host is cleaned and cut.
    let hostile = format!("evil\u{202E}gro.\u{200B}example{}", "a".repeat(400));
    let cleaned = card::clean_label(&hostile, card::MAX_SOURCE_LABEL);
    assert!(cleaned.len() <= card::MAX_SOURCE_LABEL && !cleaned.chars().any(card::is_hostile_char));
    let mut page = fixture("02-public-strong");
    page.source.as_mut().unwrap().host = hostile;
    let c = Card::from_page(&page, false).unwrap();
    assert!(c.check().is_ok());
    // check() refuses what the builder never makes.
    let mut bad = c.clone();
    bad.source_label = "a\u{202E}b".into();
    assert!(bad.check().is_err());
    let mut bad = c.clone();
    bad.headline = "h".repeat(card::MAX_HEADLINE + 1);
    assert!(bad.check().is_err());
    let mut bad = c.clone();
    bad.date = "2026-11-2".into();
    assert!(bad.check().is_err());
    let mut bad = c.clone();
    bad.link = CardLink::public("K7M2XQ9");
    assert!(bad.check().is_err());
    let mut bad = c;
    bad.gauge.level = Some(101);
    assert!(bad.check().is_err());
}

// ---------------------------------------------------------------- pages

#[test]
fn page_status_and_invariants() {
    assert_eq!(fixture("13-public-removed").http_status(), 451);
    assert_eq!(fixture("23-secret-disabled").http_status(), 451);
    assert_eq!(fixture("21-secret-deleted").http_status(), 410);
    assert_eq!(fixture("22-secret-expired").http_status(), 410);
    assert_eq!(fixture("01-public-very-strong").http_status(), 200);
    // check() refuses broken pages.
    let mut p = fixture("15-secret-text");
    p.robots = Robots::Index;
    assert!(p.check().is_err());
    let mut p = fixture("15-secret-text");
    p.canonical = Some("k7m2xq9".into());
    assert!(p.check().is_err());
    let mut p = fixture("03-public-unclear");
    p.run.as_mut().unwrap().spans = vec![Span { start: 0, end: 10, words: 30, confidence: Some(90) }];
    assert!(p.check().is_err(), "passages outside the strong bands");
    let mut p = fixture("16-secret-short-strong");
    p.run.as_mut().unwrap().confidence_text = Some("97.3%".into());
    assert!(p.check().is_err(), "a short text prints no percentage");
    let mut p = fixture("01-public-very-strong");
    p.ninja = Some(NinjaState::Bow);
    assert!(p.check().is_err(), "the ninja follows the rule");
    let mut p = fixture("01-public-very-strong");
    p.run.as_mut().unwrap().spans[0].words = 23;
    assert!(p.check().is_err(), "no passage under 24 words");
    let mut p = fixture("21-secret-deleted");
    p.run = fixture("15-secret-text").run;
    assert!(p.check().is_err(), "an ended page shows nothing");
    let mut p = fixture("01-public-very-strong");
    p.text.as_mut().unwrap().shown = Shown::Full;
    assert!(p.check().is_err(), "a public page never shows the full text");
    // The schema tag is exact.
    let s = std::fs::read_to_string(format!("{}/tests/fixtures/pages/15-secret-text.json", env!("CARGO_MANIFEST_DIR")))
        .unwrap();
    let wrong = s.replace(PAGE_SCHEMA, "slop_ninja_detect_page_v1");
    assert!(serde_json::from_str::<Page>(&wrong).is_err());
}

#[test]
fn span_offsets_are_utf16() {
    let p = fixture("15-secret-text");
    let body = p.text.as_ref().unwrap().body.as_deref().unwrap();
    let s = p.run.as_ref().unwrap().spans[0];
    let units: Vec<u16> = body.encode_utf16().collect();
    let passage = String::from_utf16(&units[s.start as usize..s.end as usize]).unwrap();
    assert!(passage.starts_with("In today's") && passage.ends_with("growing together."));
    // The emoji before the passage takes two UTF-16 units.
    assert!(body.find("In today's").unwrap() as u32 > s.start);
}

// ---------------------------------------------------------------- X posts

const X_FIXTURES: &[&str] =
    &["29-x-post-short", "32-x-post-removed", "33-x-post-gone", "35-x-post-purged", "36-x-post-pending"];

/// Every X-looking URL in `json`: the strings that hold an X host or a
/// status path.
fn x_urls(json: &str) -> Vec<String> {
    let v: serde_json::Value = serde_json::from_str(json).unwrap();
    fn walk(v: &serde_json::Value, out: &mut Vec<String>) {
        match v {
            serde_json::Value::String(s) => {
                if ["x.com/", "twitter.com", "t.co/", "/status/", "twimg"].iter().any(|n| s.contains(n)) {
                    out.push(s.clone());
                }
            }
            serde_json::Value::Array(a) => a.iter().for_each(|x| walk(x, out)),
            serde_json::Value::Object(m) => m.values().for_each(|x| walk(x, out)),
            _ => {}
        }
    }
    let mut out = Vec::new();
    walk(&v, &mut out);
    out
}

#[test]
fn x_pages_keep_their_invariants() {
    let dir = format!("{}/tests/fixtures/pages", env!("CARGO_MANIFEST_DIR"));
    for name in X_FIXTURES {
        let raw = std::fs::read_to_string(format!("{dir}/{name}.json")).unwrap();
        let p = fixture(name);
        assert!(p.check().is_ok(), "{name}");
        assert!(p.is_x_post(), "{name}");
        // No text body, anchors, quote or embed; never indexed; no ad slot.
        assert!(p.text.is_none() && p.robots == Robots::Noindex, "{name}");
        assert!(p.ads_forbidden() && p.ad_page_kind() == "x_post", "{name}");
        assert!(p.run.as_ref().is_none_or(|r| r.spans.is_empty() && r.spans_note == Some(SpansNote::XPost)));
        let keys: Vec<String> =
            serde_json::from_str::<serde_json::Map<String, serde_json::Value>>(&raw).unwrap().keys().cloned().collect();
        for k in ["embed", "quote", "parts", "author_name", "handle", "avatar", "media"] {
            assert!(!keys.contains(&k.to_owned()) && !raw.contains(&format!("\"{k}\"")), "{name}: {k}");
        }
        // The only X URL is post_url, on a live page; the source has none.
        let want: Vec<String> = p.x.map(|x| x.post_url.url()).into_iter().collect();
        assert_eq!(x_urls(&raw), want, "{name}");
        assert!(p.source.as_ref().is_none_or(|s| s.url.is_none() && !s.link), "{name}");
    }
    // Live pages link the post; ended pages say why, with their status.
    assert_eq!(fixture("29-x-post-short").x.unwrap().post_url.url(), "https://x.com/i/status/2");
    let p = fixture("29-x-post-short");
    assert!(p.x.is_some() && p.http_status() == 200 && p.receipt.is_some());
    for (name, code, status) in [
        ("32-x-post-removed", RefusalCode::XRemoved, 451),
        ("33-x-post-gone", RefusalCode::XGone, 410),
        ("35-x-post-purged", RefusalCode::XPurged, 410),
    ] {
        let p = fixture(name);
        assert_eq!((p.ended.map(|e| e.code), p.http_status()), (Some(code), status), "{name}");
        assert!(p.x.is_none() && p.run.is_none() && p.source.is_none(), "{name}");
    }
    assert!(fixture("32-x-post-removed").removal.is_some_and(|r| r.scope == RemovalScope::Page));
    // Pages that are not X pages never carry x or an ended reason.
    for name in ["01-public-very-strong", "13-public-removed", "15-secret-text", "21-secret-deleted"] {
        let p = fixture(name);
        assert!(p.x.is_none() && p.ended.is_none() && !p.is_x_post(), "{name}");
    }
    assert_eq!(fixture("01-public-very-strong").ad_page_kind(), "public_result");
    assert!(!fixture("01-public-very-strong").ads_forbidden());
    assert!(fixture("15-secret-text").ads_forbidden() && fixture("24-paid-document").ads_forbidden());
    assert!(fixture("27-unstored").ads_forbidden() && !fixture("28-example").ads_forbidden());
}

#[test]
fn check_refuses_broken_x_pages() {
    let refused = |p: Page, why: &str| assert!(p.check().is_err(), "{why}");
    let mut p = fixture("29-x-post-short");
    p.text = fixture("16-secret-short-strong").text;
    refused(p, "an X page has no text body");
    let mut p = fixture("29-x-post-short");
    p.text = Some(page::Text {
        body: None,
        shown: Shown::Withheld,
        anchors: vec![Anchor {
            start: 0,
            end: 0,
            role: AnchorRole::Passage,
            from_word: 1,
            to_word: 30,
            fragment: None,
        }],
        further: Vec::new(),
        page_words: None,
        withheld_reason: Some(WithheldReason::Unranked),
    });
    refused(p, "an X page has no anchors");
    let mut p = fixture("29-x-post-short");
    p.robots = Robots::Index;
    refused(p, "an X page is noindex");
    let mut p = fixture("29-x-post-short");
    p.run.as_mut().unwrap().spans = vec![Span { start: 0, end: 10, words: 30, confidence: None }];
    refused(p, "an X page draws no passage");
    let mut p = fixture("29-x-post-short");
    p.run.as_mut().unwrap().spans_note = Some(SpansNote::StrongBandsOnly);
    refused(p, "an X page says why no passage is drawn");
    // post_url is the only X URL: the source carries none and links nothing.
    let mut p = fixture("29-x-post-short");
    p.source.as_mut().unwrap().url = Some("https://x.com/i/status/2".into());
    refused(p, "an X source has no URL, even the canonical one");
    let mut p = fixture("29-x-post-short");
    p.source.as_mut().unwrap().link = true;
    refused(p, "an X source links nothing");
    let mut p = fixture("29-x-post-short");
    p.source.as_mut().unwrap().title = Some("A post".into());
    refused(p, "an X source keeps no title");
    let mut p = fixture("29-x-post-short");
    p.source.as_mut().unwrap().author = Some("someone".into());
    refused(p, "an X source keeps no author");
    let mut p = fixture("29-x-post-short");
    p.source.as_mut().unwrap().via = SourceVia::Web;
    refused(p, "an X source is read through X");
    // x is exactly on a live X page.
    let mut p = fixture("29-x-post-short");
    p.x = None;
    refused(p, "a live X page links the post");
    let mut p = fixture("33-x-post-gone");
    p.x = fixture("29-x-post-short").x;
    refused(p, "an ended X page has no link");
    let mut p = fixture("36-x-post-pending");
    p.x = fixture("29-x-post-short").x;
    refused(p, "a pending X page has no link");
    let mut p = fixture("01-public-very-strong");
    p.x = fixture("29-x-post-short").x;
    refused(p, "x only on an X page");
    let mut p = fixture("29-x-post-short");
    p.state = DetState::ExcerptWithheld;
    p.removal = fixture("08-public-excerpt-withheld").removal;
    p.x = None;
    refused(p, "an X page has no excerpt to withhold");
    let mut p = fixture("15-secret-text");
    p.platform = Platform::X;
    refused(p, "an X post is a public page");
    // Each ended code on its own state, on X pages only, and always present.
    for (name, code) in [
        ("33-x-post-gone", RefusalCode::XRemoved),
        ("32-x-post-removed", RefusalCode::XGone),
        ("32-x-post-removed", RefusalCode::XPurged),
        ("35-x-post-purged", RefusalCode::XRemoved),
        ("33-x-post-gone", RefusalCode::PageGone),
    ] {
        let mut p = fixture(name);
        p.ended = Some(page::Ended { code });
        refused(p, "each ended code on its own state");
    }
    let mut p = fixture("33-x-post-gone");
    p.state = DetState::Expired;
    refused(p, "x_gone ends a deleted page");
    let mut p = fixture("21-secret-deleted");
    p.ended = Some(page::Ended { code: RefusalCode::XGone });
    refused(p, "an ended reason only on an X page");
    let mut p = fixture("13-public-removed");
    p.ended = Some(page::Ended { code: RefusalCode::XRemoved });
    refused(p, "x_removed only on an X page");
    let mut p = fixture("29-x-post-short");
    p.ended = Some(page::Ended { code: RefusalCode::XGone });
    refused(p, "an ended reason only on an ended page");
    for name in ["33-x-post-gone", "32-x-post-removed"] {
        let mut p = fixture(name);
        p.ended = None;
        refused(p, "an ended X page names why");
    }
    let mut p = fixture("32-x-post-removed");
    p.removal = None;
    refused(p, "a removed X page names its removal");
}

#[test]
fn x_delete_holds_only_a_source_id_and_its_cause() {
    use slop_ninja_detect_page::queue::{Job, QueueMessage};
    // The pattern names every field and no `..`, and each binding is typed:
    // adding a field to the variant, or changing a type, breaks this test.
    fn fields(job: &Job) -> Option<(u64, XDeleteCause)> {
        match *job {
            Job::XDelete { source_id, cause } => {
                let id: u64 = source_id;
                let cause: XDeleteCause = cause;
                Some((id, cause))
            }
            _ => None,
        }
    }
    // The causes are exactly these two, each a refusal code its detections
    // end with, on its own state.
    let causes: Vec<&str> = XDeleteCause::ALL.iter().map(|c| c.as_str()).collect();
    assert_eq!(causes, ["x_gone", "x_removed"]);
    for c in XDeleteCause::ALL {
        assert_eq!(c.code().as_str(), c.as_str());
        assert!(page::ENDED_CODES.contains(&c.code()), "{c}");
        assert_eq!(page::Ended::state_of(c.code()), Some(c.state()), "{c}");
        assert_eq!(c.deletes_source_row(), *c != XDeleteCause::XRemoved);
    }
    // x_purged has its own pages (`x_purge_page`), never an x_delete.
    assert_eq!(XDeleteCause::parse("x_purged"), None);
    // On the wire: the version, the type, the integer and the cause, and
    // nothing else; no post id, X URL or slug.
    let dir = format!("{}/tests/fixtures/queue", env!("CARGO_MANIFEST_DIR"));
    for c in XDeleteCause::ALL {
        let name = c.as_str().trim_start_matches("x_");
        let raw = std::fs::read_to_string(format!("{dir}/x-delete-{name}.json")).unwrap();
        let v: serde_json::Map<String, serde_json::Value> = serde_json::from_str(&raw).unwrap();
        let mut keys: Vec<&str> = v.keys().map(String::as_str).collect();
        keys.sort_unstable();
        assert_eq!(keys, ["cause", "source_id", "type", "v"], "{name}");
        assert!(v["source_id"].is_u64() && v["type"] == "x_delete" && v["cause"] == c.as_str(), "{name}");
        assert!(!raw.contains("status") && !raw.contains("x.com"), "{name}");
        let m: QueueMessage = serde_json::from_str(&raw).unwrap();
        assert_eq!(fields(&m.job), Some((42, *c)));
    }
    // A cause outside the four, or a post id in place of the row id, does
    // not parse.
    assert!(
        serde_json::from_str::<QueueMessage>(r#"{"v":1,"type":"x_delete","source_id":42,"cause":"x_purged"}"#).is_err()
    );
    assert!(
        serde_json::from_str::<QueueMessage>(r#"{"v":1,"type":"x_delete","source_id":"x:status:1","cause":"x_gone"}"#)
            .is_err()
    );
    assert!(serde_json::from_str::<QueueMessage>(r#"{"v":1,"type":"x_delete","cause":"x_gone"}"#).is_err());
}

#[test]
fn paid_open_rides_only_on_a_cut_unstored_result() {
    let refused = |p: Page, why: &str| assert!(p.check().is_err(), "{why}");
    for name in ["27-unstored", "39-unstored-paid-open"] {
        let p = fixture(name);
        assert!(p.check().is_ok() && p.paid_open.is_some(), "{name}");
        let mut q = p.clone();
        q.paid_open = None;
        refused(q, "a cut unstored result says whether paid checks are open");
        let mut q = p;
        q.run.as_mut().unwrap().cut = CutKind::None;
        refused(q, "no paid offer for a text that was not cut");
    }
    for name in ["01-public-very-strong", "20-secret-free-cut", "28-example", "29-x-post-short", "21-secret-deleted"] {
        let mut p = fixture(name);
        assert_eq!(p.paid_open, None, "{name}");
        p.paid_open = Some(true);
        refused(p, "paid_open only on the unstored result");
    }
    // Every page but the two unstored fixtures offers nothing.
    let reader = PaidReader { billing: BillingState::Grants, paid_preview: true };
    assert_eq!(fixture("20-secret-free-cut").paid_offer(Some(reader)), PaidOffer::Nothing);
    assert_eq!(fixture("39-unstored-paid-open").paid_offer(Some(reader)), PaidOffer::CheckAll);
    assert_eq!(fixture("27-unstored").paid_offer(None), PaidOffer::Nothing);
    assert_eq!(fixture("39-unstored-paid-open").paid_offer(None), PaidOffer::SignIn);
}

#[test]
fn post_urls_take_only_the_canonical_form() {
    use slop_ninja_detect_page::x::*;
    assert_eq!(x_canonical(1).as_deref(), Some("https://x.com/i/status/1"));
    assert_eq!(x_canonical(0), None);
    assert_eq!(x_canonical(i64::MAX as u64).as_deref(), Some("https://x.com/i/status/9223372036854775807"));
    assert_eq!(x_canonical(i64::MAX as u64 + 1), None);
    assert_eq!(parse_x_canonical("https://x.com/i/status/1855"), Some(1855));
    assert_eq!(parse_x_canonical("https://x.com/i/status/9223372036854775807"), Some(i64::MAX as u64));
    assert_eq!(x_source_key(1855), "x:status:1855");
    let bad = [
        "https://x.com/i/status/9223372036854775808",
        "https://x.com/i/status/0",
        "https://x.com/i/status/01",
        "https://x.com/i/status/",
        "https://x.com/i/status/12/",
        "https://x.com/i/status/12?s=20",
        "https://x.com/i/status/12#m",
        "https://x.com/a/status/12",
        "https://x.com/i/web/status/12",
        "https://twitter.com/i/status/12",
        "https://www.x.com/i/status/12",
        "https://X.com/i/status/12",
        "http://x.com/i/status/12",
        "x.com/i/status/12",
        "https://x.com/i/status/+12",
        "https://x.com/i/status/ 12",
        "https://x.com/i/status/12345678901234567890",
        "https://x.com/i/status/1\"><script>",
    ];
    for b in bad {
        assert_eq!(parse_x_canonical(b), None, "{b}");
        // The page's type refuses it at parse time.
        let json = serde_json::to_string(b).unwrap();
        assert!(serde_json::from_str::<PostUrl>(&json).is_err(), "{b}");
        assert!(serde_json::from_str::<XPost>(&format!("{{\"post_url\":{json}}}")).is_err(), "{b}");
    }
    let u: PostUrl = serde_json::from_str("\"https://x.com/i/status/1855\"").unwrap();
    assert_eq!((u.id(), u.url(), u.to_string()), (1855, "https://x.com/i/status/1855".into(), u.url()));
    assert_eq!(serde_json::to_string(&u).unwrap(), "\"https://x.com/i/status/1855\"");
    assert_eq!(PostUrl::new(0), None);
    assert_eq!(serde_json::to_string(&XPost::new(2).unwrap()).unwrap(), r#"{"post_url":"https://x.com/i/status/2"}"#);
    // A refusal's url is typed the same way.
    let r = r#"{"state":"refused","code":"x_post_link","url":"https://x.com/a/status/2"}"#;
    assert!(serde_json::from_str::<ApiRefusal>(r).is_err());
}

#[test]
fn the_op_route_takes_the_bare_envelope() {
    // Both op routes take the signed envelope alone: a body with anything
    // beside it (an X link included) does not parse, so no op carries a
    // post's link outside its signature either.
    use slop_ninja_signed_op as op;
    let key = op::SigningKey::from_bytes(&[7; 32]);
    let now = 1_795_169_040_000;
    let mut args = serde_json::Map::new();
    args.insert("case_id".into(), "td-8k2m4q7x".into());
    let signed = op::Op {
        target: op::Target::Detect,
        env: op::Env::Staging,
        issued_ms: now,
        expires_ms: now + 60_000,
        nonce: op::Nonce([3; 16]),
        op: "restore".into(),
        args,
    };
    let wire = serde_json::to_value(op::sign(&signed, &key).unwrap()).unwrap();
    let verifier = op::Verifier::new(
        op::OperatorKey::from_verifying_key(key.verifying_key()),
        op::Env::Staging,
        &[op::Target::Detect, op::Target::Edge],
    );
    assert!(verifier.verify(&serde_json::to_vec(&wire).unwrap(), now + 1).is_ok());
    let mut beside = wire.clone();
    beside["x_url"] = "https://x.com/i/status/2".into();
    let wrapped = serde_json::json!({"signed": wire, "x_url": "https://x.com/i/status/2"});
    for body in [beside, wrapped] {
        assert_eq!(verifier.verify(&serde_json::to_vec(&body).unwrap(), now + 1), Err(op::Refusal::Malformed));
    }
    // The catalog's X ops: the removal by hash, the class purge; never the
    // withdrawn oEmbed trust reset.
    assert!(op::in_catalog(op::Target::Detect, "x_remove") && op::in_catalog(op::Target::Detect, "x_purge"));
    assert!(!op::CATALOG.iter().any(|(_, o)| *o == ["x_oembed", "_trust"].concat()));
}

#[test]
fn the_retired_x_names_are_gone() {
    // Pasted text is ordinary paste whatever its source, and an X page shows
    // no part of the post: none of the retired x.com-source rule's names, and
    // none of the static quote's, survive in the contracts, fixtures or
    // schemas.
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let banned = [
        ["text", "_withheld"].concat(),
        ["X_", "DEVELOPER"].concat(),
        ["ADV", "16"].concat(),
        ["x_source", "_rule"].concat(),
        ["XSource", "Rule"].concat(),
        ["X", "Embed"].concat(),
        ["X", "Part"].concat(),
        ["X", "View"].concat(),
        ["x", "embed"].concat(),
        ["\"em", "bed\""].concat(),
        ["is_x", "_href"].concat(),
        ["no", "_store"].concat(),
        ["XPost", "Url"].concat(),
        ["Op", "Request"].concat(),
        ["\"x", "_url\""].concat(),
        // The paid-read design of X posts (replaced by the free embed): no
        // API read, budget, distrust, candidate or edit tracking, and no
        // post link beside a signed op.
        ["x_oembed", "_trust"].concat(),
        ["X_READ", "_BUDGET"].concat(),
        ["X_BEARER", "_TOKEN"].concat(),
        ["X_MAR", "KED"].concat(),
        ["X_EXIS", "TENCE"].concat(),
        ["edit_check", "_at"].concat(),
        ["gone_candidate", "_at"].concat(),
        ["x_mismatch", "_at"].concat(),
        ["x_post", "_superseded"].concat(),
        ["x_", "edited"].concat(),
        ["x_un", "checked"].concat(),
        ["XCheck", "Why"].concat(),
    ];
    fn walk(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
        for e in std::fs::read_dir(dir).unwrap() {
            let p = e.unwrap().path();
            if p.is_dir() { walk(&p, out) } else { out.push(p) }
        }
    }
    let mut files = Vec::new();
    walk(&root.join("src"), &mut files);
    walk(&root.join("tests/fixtures"), &mut files);
    walk(&root.join("../account-wire/src"), &mut files);
    walk(&root.join("../../detect-worker/migrations"), &mut files);
    walk(&root.join("../../account-worker/migrations"), &mut files);
    walk(&root.join("../signed-op/src"), &mut files);
    assert!(files.len() > 100);
    for f in files {
        let s = std::fs::read_to_string(&f).unwrap();
        for b in &banned {
            assert!(!s.contains(b.as_str()), "{} names {b}", f.display());
        }
    }
}

// ---------------------------------------------------------------- headers

#[test]
fn principal_codec() {
    let p = Principal { user_id: UserId([0xab; 16]), auth_time_ms: 1_795_169_040_000, flags: sn::flags::PAID_PREVIEW };
    let s = p.encode();
    assert_eq!(s, "v1;abababababababababababababababab;1795169040000;1");
    assert_eq!(Principal::decode(&s), Some(p));
    assert!(p.has(sn::flags::PAID_PREVIEW));
    for bad in [
        "",
        "v2;abababababababababababababababab;1;0",
        "v1;ABABABABABABABABABABABABABABABAB;1;0",
        "v1;abab;1;0",
        "v1;abababababababababababababababab;01;0",
        "v1;abababababababababababababababab;-1;0",
        "v1;abababababababababababababababab;1;00",
        "v1;abababababababababababababababab;1;G",
        "v1;abababababababababababababababab;1;123456789",
        "v1;abababababababababababababababab;1;0;x",
        "v1;abababababababababababababababab;1",
        "v1;abababababababababababababababab; 1;0",
    ] {
        assert_eq!(Principal::decode(bad), None, "{bad:?}");
    }
    assert_eq!(Principal::decode("v1;abababababababababababababababab;0;0").unwrap().auth_time_ms, 0);
}

#[test]
fn routes_and_headers() {
    assert!(sn::is_sn_header("X-SN-Principal") && sn::is_sn_header("x-sn-anything") && !sn::is_sn_header("x-s"));
    assert!(!sn::is_sn_header("cookie"));
    assert!(sn::internal_request_allowed(false, false));
    assert!(!sn::internal_request_allowed(true, false) && !sn::internal_request_allowed(false, true));
    let mut ids = HashSet::new();
    let mut paths = HashSet::new();
    for r in RouteId::ALL {
        let s = r.spec();
        assert_eq!(s.id, *r);
        assert!(ids.insert(r.as_str()), "{}", r.as_str());
        assert!(paths.insert((s.method, s.path)), "{}", s.path);
        assert_eq!(RouteId::parse(r.as_str()), Some(*r));
        // Principals only on the site host's named routes.
        if s.principal {
            assert!(s.hosts.admits(HostRole::Site) && !s.hosts.admits(HostRole::Secret));
        }
        // No CSRF check on GETs, the signed op routes, the paid API or webhooks.
        if s.method == "GET"
            || matches!(r, RouteId::DetectOp | RouteId::AccountOp | RouteId::PaidApi | RouteId::BillingWebhook)
        {
            assert!(!s.csrf, "{}", r.as_str());
        }
    }
    let principal: Vec<_> = RouteId::ALL.iter().filter(|r| r.takes_principal()).map(|r| r.as_str()).collect();
    assert_eq!(principal, ["share", "paid_quote", "paid_start", "removal"]);
    for r in [RouteId::Manage, RouteId::Job] {
        assert!(r.spec().hosts.admits(HostRole::Secret) && !r.spec().hosts.admits(HostRole::Site));
    }
    let internal: Vec<_> = InternalRoute::ALL.iter().filter(|r| r.takes_principal()).collect();
    assert_eq!(internal, [&InternalRoute::Owner]);
    for r in InternalRoute::ALL {
        assert!(r.method_path().1.starts_with("/internal/v1/"));
    }
}

#[test]
fn origins_from_config() {
    let o = test_origins();
    assert_eq!(o.site.host(), "site.example");
    assert_eq!(o.secret.host(), "s.site.example");
    assert_eq!(o.sn_host(HostRole::Secret), "s.site.example");
    assert_eq!(o.role_of_sn_host("site.example"), Some(HostRole::Site));
    assert_eq!(o.role_of_sn_host("SITE.EXAMPLE"), None);
    assert_eq!(o.secret_page_url("7hq2kd9m4xw8rt3vn6pb"), "https://s.site.example/7hq2kd9m4xw8rt3vn6pb");
    assert_eq!(o.public_page_url("k7m2xq9"), "https://site.example/d/k7m2xq9");
    let local = Origins::from_vars("https://site.localhost:8787", "https://secret.localhost:8788").unwrap();
    assert_eq!(local.site.host(), "site.localhost");
    assert_eq!(local.sn_host(HostRole::Site), "site.localhost:8787");
    use origins::OriginError::*;
    for (bad, err) in [
        ("http://site.example", Form),
        ("https://site.example/", Form),
        ("https://site.example?x", Form),
        ("https://user@site.example", Form),
        ("https://Site.example", Host),
        ("https://site..example", Host),
        ("https://-site.example", Host),
        ("https://site.example.", Host),
        ("https://site.example:0", Port),
        ("https://site.example:080", Port),
        ("https://site.example:65536", Port),
    ] {
        assert_eq!(Origin::parse(bad), Err(err), "{bad}");
    }
    assert_eq!(Origins::from_vars("https://a.example", "https://a.example:444"), Err(SameHost));
}

// ---------------------------------------------------------------- codes

#[test]
fn refusal_codes() {
    let mut seen = HashSet::new();
    for c in RefusalCode::ALL {
        assert!(seen.insert(c.as_str()), "{}", c.as_str());
        assert_eq!(RefusalCode::parse(c.as_str()), Some(*c));
        assert_eq!(c.copy_key(), format!("refusal.{}", c.as_str()));
        assert!((400..=599).contains(&c.status()) || c.status() == 451);
    }
    // The design's named codes.
    for s in ["link_unavailable", "rerun_gone", "rerun_blocked", "rerun_unavailable", "rerun_not_needed"] {
        assert!(RefusalCode::parse(s).is_some(), "{s}");
    }
    assert_eq!(RefusalCode::RerunGone.status(), 410);
    assert_eq!(RefusalCode::RerunBlocked.status(), 451);
    // Every op-route refusal is a refusal code with the same status.
    for r in slop_ninja_signed_op::Refusal::ALL {
        let c = RefusalCode::parse(r.code()).unwrap_or_else(|| panic!("{} missing", r.code()));
        assert_eq!(c.status(), r.status(), "{}", r.code());
    }
    // `/signin/?e=` codes.
    let signin: Vec<_> = RefusalCode::ALL.iter().filter(|c| c.is_signin_error()).map(|c| c.as_str()).collect();
    for s in [
        "state",
        "expired",
        "denied",
        "provider",
        "method_off",
        "email_in_use",
        "throttled",
        "wallet_type",
        "signature",
        "link_wrong_browser",
        "step_up_identity",
        "session",
    ] {
        assert!(signin.contains(&s), "{s}");
    }
    // The X codes, their statuses and their surface.
    for (s, status) in [
        ("x_post_link", 422),
        ("x_off", 503),
        ("x_not_a_post", 422),
        ("x_post_unavailable", 422),
        ("x_post_long", 422),
        ("x_post_too_new", 422),
        ("x_busy", 503),
        ("x_unavailable", 503),
        ("x_removed", 422),
        ("x_gone", 410),
        ("x_purged", 410),
    ] {
        let c = RefusalCode::parse(s).unwrap_or_else(|| panic!("{s}"));
        assert_eq!((c.status(), c.group()), (status, refusal::Group::X), "{s}");
        assert_eq!(c.copy_key(), format!("refusal.{s}"));
    }
    let x: Vec<_> = RefusalCode::ALL.iter().filter(|c| c.group() == refusal::Group::X).collect();
    assert_eq!(x.len(), 11);
    // x_post_link carries the canonical post URL, and only it carries a url.
    let link = ApiRefusal::x_post_link(1855).unwrap();
    assert_eq!(
        serde_json::to_string(&link).unwrap(),
        r#"{"state":"refused","code":"x_post_link","url":"https://x.com/i/status/1855"}"#
    );
    assert!(link.check().is_ok());
    assert_eq!(ApiRefusal::x_post_link(0), None);
    assert!(ApiRefusal::new(RefusalCode::XPostLink).check().is_err());
    assert_eq!(refusal::URL_CODES, &[RefusalCode::XPostLink]);
    // x_post_too_new carries when the post can be checked, and only it does.
    let young = ApiRefusal::x_post_too_new(1_795_172_700_000);
    assert_eq!(
        serde_json::to_string(&young).unwrap(),
        r#"{"state":"refused","code":"x_post_too_new","after_ms":1795172700000}"#
    );
    assert!(young.check().is_ok());
    assert!(ApiRefusal::new(RefusalCode::XPostTooNew).check().is_err());
    let mut bad = ApiRefusal::new(RefusalCode::XPostLong);
    bad.after_ms = Some(1);
    assert!(bad.check().is_err());
    let mut bad = ApiRefusal::new(RefusalCode::XOff);
    bad.url = link.url;
    assert!(bad.check().is_err());
    assert!(ApiRefusal::new(RefusalCode::XGone).check().is_ok());
    // The ended codes, each on its one state.
    for c in page::ENDED_CODES {
        assert_eq!(c.group(), refusal::Group::X);
        let want = if *c == RefusalCode::XRemoved { DetState::Removed } else { DetState::Deleted };
        assert_eq!(page::Ended::state_of(*c), Some(want), "{c}");
    }
    assert_eq!(page::Ended::state_of(RefusalCode::PageGone), None);
    assert_eq!(page::ENDED_CODES.len(), 3);
    // A live page whose run document is missing: 503, not an X code.
    let ru = RefusalCode::parse("report_unavailable").unwrap();
    assert_eq!((ru.status(), ru.group(), ru.copy_key()), (503, refusal::Group::Page, "refusal.report_unavailable"));
    assert_eq!(page::Ended::state_of(ru), None);
    let r = serde_json::to_string(&ApiRefusal::new(RefusalCode::Busy)).unwrap();
    assert_eq!(r, r#"{"state":"refused","code":"busy"}"#);
    assert!(serde_json::from_str::<ApiRefusal>(r#"{"state":"error","code":"busy"}"#).is_err());
    assert!(serde_json::from_str::<ApiRefusal>(r#"{"state":"refused","code":"nope"}"#).is_err());
}

#[test]
fn code_sets_parse_only_their_strings() {
    assert_eq!(Band::parse("very_strong"), Some(Band::VeryStrong));
    assert_eq!(Band::parse("no_strong_signs"), None);
    assert_eq!(serde_json::to_string(&LengthBin::Long).unwrap(), "\"451_1000\"");
    assert!(serde_json::from_str::<Kind>("\"Public_url\"").is_err());
    assert_eq!(Expiry::Week.duration_ms(), Some(7 * 86_400_000));
    assert_eq!(Expiry::Keep.duration_ms(), None);
    assert_eq!(LengthBin::of_words(23), None);
    assert_eq!(LengthBin::of_words(24), Some(LengthBin::Short24));
    assert_eq!(LengthBin::of_words(150), Some(LengthBin::Measured));
    assert_eq!(LengthBin::of_words(1001), None);
    assert_eq!(Band::Weak.number(), 1);
    assert_eq!(Band::VeryStrong.number(), 4);
    assert!(NinjaState::Bow < NinjaState::Stoic && NinjaState::Ready < NinjaState::Strike);
    assert_eq!(SpansNote::parse("x_post"), Some(SpansNote::XPost));
    assert_eq!(XDeleteCause::ALL.iter().map(|c| c.as_str()).collect::<Vec<_>>(), ["x_gone", "x_removed"]);
    assert_eq!(XDeleteCause::parse("x_edited"), None);
    assert!(CaseKind::ALL.iter().filter(|k| !k.is_form_track()).eq([CaseKind::XRequest].iter()));
}

#[test]
fn switches_and_their_effective_state() {
    use switches::*;
    let mut names = HashSet::new();
    for s in SWITCHES {
        assert!(names.insert(s.name));
        assert!(s.states.contains(&s.off));
        assert_eq!(parse_var(s, ""), Some(s.states[0]));
        for st in s.states {
            // A var names any state but a row-only one.
            let want = (!s.beyond_off.contains(st)).then_some(*st);
            assert_eq!(parse_var(s, st), want);
        }
        assert!(s.beyond_off.iter().all(|b| s.states.contains(b) && *b != s.off));
    }
    let tool = switch("DETECT_TOOL").unwrap();
    assert_eq!(effective(tool, "on", Some("off")), "off");
    assert_eq!(effective(tool, "off", Some("on")), "off", "the var is the hard off");
    // A store that cannot be read: the var decides.
    assert_eq!(effective(tool, "on", None), "on");
    // An unset var is the default; a var that names no state forces the off
    // state, so a typo never turns a switch on.
    assert_eq!(effective(tool, "", None), "on");
    assert_eq!(effective(tool, "anything", None), "off");
    assert_eq!(parse_var(tool, "anything"), None);
    let pages = switch("DETECT_PAGES").unwrap();
    assert_eq!(effective(pages, "Off", Some("on")), "off");
    assert_eq!(effective(pages, "OFF", None), "off");
    let paid = switch("DETECT_PAID").unwrap();
    assert_eq!(effective(paid, "", None), "off", "default off");
    // DETECT_X: off by default, on only by var and row together; purged
    // (the row x_purge writes) holds even under the var's hard off, and a
    // var cannot name it.
    let x = switch("DETECT_X").unwrap();
    assert_eq!((x.states, x.off, x.owner), (&["off", "on", "purged"][..], "off", Owner::Detect));
    let want: Vec<&str> = DetectXState::ALL.iter().map(|s| s.as_str()).collect();
    assert_eq!(x.states, &want[..]);
    assert_eq!(effective(x, "", None), "off");
    assert_eq!(effective(x, "on", Some("on")), "on");
    assert_eq!(effective(x, "on", Some("off")), "off");
    assert_eq!(effective(x, "off", Some("on")), "off");
    assert_eq!(effective(x, "on", Some("purged")), "purged");
    assert_eq!(effective(x, "off", Some("purged")), "purged");
    assert_eq!(effective(x, "", Some("purged")), "purged");
    assert_eq!(effective(x, "purged", None), "off");
    assert_eq!(effective(x, "purged", Some("on")), "off");
    // set_switch can always move the row back.
    assert_eq!(effective(x, "on", Some("off")), "off");
    assert!(SWITCHES.iter().filter(|s| !s.beyond_off.is_empty()).map(|s| s.name).eq(["DETECT_X"]));
    let billing = switch("BILLING").unwrap();
    assert_eq!(effective(billing, "grants", Some("grants")), "grants");
    assert_eq!(effective(billing, "off", Some("grants")), "off");
    assert_eq!(effective(billing, "grants", Some("bogus")), "grants");
    assert_eq!(effective(billing, "Live", Some("live")), "off");
    let accounts = switch("ACCOUNTS").unwrap();
    assert_eq!(effective(accounts, "read-only", Some("on")), "read_only");
    assert!(switch("auth.x").is_some() && switch("nope").is_none());
}

#[test]
fn metric_points_are_named_once() {
    for set in [ae::web::ALL, ae::detect::ALL, ae::account::ALL] {
        let mut names = HashSet::new();
        for p in set {
            assert!(names.insert(p.name), "{}", p.name);
            assert!(!p.blobs.is_empty());
            for f in p.blobs {
                for banned in ["slug", "email", "user", "token", "text", "ip"] {
                    assert!(!f.contains(banned), "{}.{f}", p.name);
                }
                // No post id, handle or URL either (as whole name parts).
                for part in f.split('_') {
                    assert!(!["id", "handle", "url", "post"].contains(&part), "{}.{f}", p.name);
                }
            }
        }
    }
    // The X points carry an outcome (or a step) only, never a post id.
    assert_eq!(ae::detect::XREAD, ae::Point { name: "xread", blobs: &["outcome"] });
    assert_eq!(ae::detect::XCHECK, ae::Point { name: "xcheck", blobs: &["outcome"] });
    assert_eq!(ae::detect::XSWEEP, ae::Point { name: "xsweep", blobs: &["step"] });
    assert!(!ae::detect::ALL.iter().any(|p| p.name.contains("embed")));
    assert_eq!(ae::detect::XREAD_OUTCOMES, &["text", "long", "too_new", "unavailable", "busy", "unexpected"]);
    assert_eq!(ae::detect::XCHECK_OUTCOMES, &["present", "gone", "unexpected", "failed", "not_checked", "control"]);
    for set in [ae::detect::XREAD_OUTCOMES, ae::detect::XCHECK_OUTCOMES, ae::detect::XSWEEP_STEPS] {
        assert_eq!(set.iter().collect::<HashSet<_>>().len(), set.len());
    }
    // Alert names are unique across datasets.
    let mut alerts = HashSet::new();
    for a in ae::detect::ALERTS.iter().chain(ae::account::ALERTS) {
        assert!(alerts.insert(*a), "{a}");
    }
    assert_eq!(ae::detect::ALERTS, &["x_check_unexpected", "x_sweep_late"]);
}
