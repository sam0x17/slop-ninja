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
    assert_eq!(GaugeEnds::default(), GaugeEnds::Emoji);
    assert_eq!(serde_json::to_string(&GaugeEnds::Letters).unwrap(), "\"letters\"");
    assert_eq!(GaugeEnds::LETTERS, ("H", "S"));
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
}

#[test]
fn switches_and_their_effective_state() {
    use switches::*;
    let mut names = HashSet::new();
    for s in SWITCHES {
        assert!(names.insert(s.name));
        assert!(s.states.contains(&s.off));
    }
    let tool = switch("DETECT_TOOL").unwrap();
    assert_eq!(effective(tool, "on", Some("off")), "off");
    assert_eq!(effective(tool, "off", Some("on")), "off", "the var is the hard off");
    // A store that cannot be read: the var decides.
    assert_eq!(effective(tool, "on", None), "on");
    assert_eq!(effective(tool, "anything", None), "on");
    let paid = switch("DETECT_PAID").unwrap();
    assert_eq!(effective(paid, "", None), "off", "default off");
    let billing = switch("BILLING").unwrap();
    assert_eq!(effective(billing, "grants", Some("grants")), "grants");
    assert_eq!(effective(billing, "off", Some("grants")), "off");
    assert_eq!(effective(billing, "grants", Some("bogus")), "grants");
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
            }
        }
    }
}
