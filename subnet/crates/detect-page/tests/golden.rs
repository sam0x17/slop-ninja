//! Golden JSON fixtures of every contract in this crate.
//!
//! Each fixture is built here from typed values, checked (`Page::check`,
//! `Card::check`), round-tripped through serde, and compared byte for byte
//! with its file under `tests/fixtures/`. The files are what the template,
//! card renderer, website and test harness consume. Every text is synthetic and every
//! source host is under the reserved `.example` domain.
//!
//! To rewrite the files after a deliberate contract change:
//! `SLOP_NINJA_BLESS=1 cargo test --test golden`.

#![allow(clippy::vec_init_then_push)]

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Debug;
use std::path::{Path, PathBuf};

use serde::Serialize;
use serde::de::DeserializeOwned;
use slop_ninja_detect_page::api::*;
use slop_ninja_detect_page::card::Card;
use slop_ninja_detect_page::internal::*;
use slop_ninja_detect_page::page::*;
use slop_ninja_detect_page::queue::{Job, QueueMessage};
use slop_ninja_detect_page::*;

// ------------------------------------------------------------------ shared

/// 2026-11-20T10:04:00Z.
pub const T0: u64 = 1_795_169_040_000;
const MIN: u64 = 60_000;
const DAY: u64 = 86_400_000;
const D24: &str = "e2536d12dff593d7b11895b3cbf6b10232601abfbebde248325afd707722b5e8";
const SCORER: &str = "7030a86f";
const PUBLIC: &str = "k7m2xq9";
const NEWER: &str = "p4r8tz2";
const SECRET: &str = "7hq2kd9m4xw8rt3vn6pb";
const SECRET2: &str = "c3jf8kq1zw5ty7hn2mdx";
const RECEIPT: &str = "0f1e2d3c4b5a69788796a5b4c3d2e1f0.very_strong.150_450.00112233445566778899aabbccddeeff";

/// An AI-like passage of 31 words.
const P1: &str = "In today's fast-paced world, community gardens serve as vibrant hubs that foster connection, promote \
sustainability, and empower residents to take meaningful ownership of their shared green spaces and local food systems.";
/// Another of 29 words.
const P2: &str = "Moreover, by leveraging innovative composting strategies and data-driven planting schedules, organizers \
can unlock unprecedented yields while ensuring that every volunteer feels seen, valued, and inspired to keep growing together.";
const H1: &str = "We planted the tomatoes late this year because the frost hung around until the second week of May.";
const H2: &str = "Marta says the beans did better on the north fence, and for once I think she is right.";
const H3: &str = "The rain barrel overflowed twice in June, so we moved it under the shed gutter.";
const H4: &str = "Next year we want a proper cold frame, a second compost bay, and a sign-up sheet that people actually read before \
they water.";
const H5: &str = "The kids from the school down the road painted labels for every bed, which helped more than any of our \
spreadsheets ever did.";
const H6: &str =
    "If you have spare seedlings in April, bring them to the Saturday swap; we always run short of peppers and basil.";

fn serving(differs: bool) -> Serving {
    Serving {
        package_label: "span package d24".into(),
        package_pub_short: D24[..8].into(),
        publication_pending: true,
        differs_from_run: differs,
    }
}

fn package() -> PackageLine {
    PackageLine { label: "span package d24".into(), published_digest: D24.into(), publication_pending: true }
}

fn words(s: &str) -> u32 {
    s.split_whitespace().count() as u32
}

/// The UTF-16 range of `needle` in `body`.
fn range(body: &str, needle: &str) -> (u32, u32) {
    let at = body.find(needle).unwrap_or_else(|| panic!("{needle:?} not in body"));
    let start = utf16_len(&body[..at]);
    (start, start + utf16_len(needle))
}

fn span(body: &str, needle: &str, confidence: Option<u8>) -> Span {
    let (start, end) = range(body, needle);
    Span { start, end, words: words(needle), confidence }
}

fn banded_run(band: Band, conf_tenths: u16, conf_text: &str, share: u8, w: u32, total: u32, cut: CutKind) -> Run {
    Run {
        scored_at: T0,
        package: package(),
        scorer_build: SCORER.into(),
        policy: "long-v1".into(),
        evidence: "evidence-d24-web-v1".into(),
        band: Some(band),
        short: None,
        confidence_tenths: Some(conf_tenths),
        confidence_text: Some(conf_text.into()),
        share_pct: Some(share),
        share_text: Some(if share == 0 { "below 1%".into() } else { format!("about {share}%") }),
        words: w,
        total_words: total,
        cut,
        tokens: w * 4 / 3,
        length: LengthBin::of_words(w),
        spans: Vec::new(),
        spans_note: if band.is_strong() { None } else { Some(SpansNote::StrongBandsOnly) },
        markers: Vec::new(),
        invisible: false,
    }
}

fn short_run(r: ShortResult, w: u32) -> Run {
    Run {
        band: None,
        short: Some(r),
        confidence_tenths: None,
        confidence_text: None,
        share_pct: None,
        share_text: None,
        spans_note: if r == ShortResult::Strong { None } else { Some(SpansNote::StrongBandsOnly) },
        ..banded_run(Band::Strong, 0, "", 0, w, w, CutKind::None)
    }
}

fn base(kind: Kind, slug: Option<&str>, state: DetState) -> Page {
    let (host, robots, platform) = match kind {
        Kind::PublicUrl => (HostRole::Site, Robots::Index, Platform::Web),
        Kind::PrivateText | Kind::PrivatePaid => (HostRole::Secret, Robots::NoindexAll, Platform::Paste),
        Kind::Unstored | Kind::Example => (HostRole::Site, Robots::Noindex, Platform::Paste),
    };
    Page {
        schema: PageSchema,
        slug: slug.map(Into::into),
        host,
        kind,
        platform,
        state,
        state_v: 1,
        created_at: if kind == Kind::Unstored { None } else { Some(T0) },
        expires_at: None,
        robots,
        canonical: if kind == Kind::PublicUrl { slug.map(Into::into) } else { None },
        superseded_by: None,
        supersedes: None,
        example_slot: None,
        serving: serving(false),
        run: None,
        sections: None,
        text: None,
        source: None,
        pending: None,
        failure: None,
        removal: None,
        card: None,
        ninja: None,
        receipt: None,
    }
}

fn source(host: &str, title: Option<&str>) -> Source {
    Source {
        host: host.into(),
        url: Some(format!("https://{host}/2026/11/garden-diary")),
        title: title.map(Into::into),
        site_name: Some("Garden Diary".into()),
        author: None,
        published_at: Some(T0 - 2 * DAY),
        fetched_at: Some(T0 - 2_000),
        via: SourceVia::Web,
        status: Some(SourceStatus::Ok),
        reputation: Some(Reputation::Ranked),
        link: true,
        checked: true,
        page_hash: Some("4f2a9c1e".into()),
        stale: false,
    }
}

fn domain_only(host: &str) -> Source {
    Source {
        url: None,
        title: None,
        site_name: None,
        published_at: None,
        fetched_at: None,
        status: None,
        reputation: None,
        link: false,
        checked: false,
        page_hash: None,
        ..source(host, None)
    }
}

fn card_ref(p: &Page, variant: CardVariant) -> CardRef {
    let image = match p.host {
        HostRole::Site => format!("/d/{}/card.png?v=1.{}", p.slug.as_deref().unwrap(), p.state_v),
        HostRole::Secret => format!("/{}/card.png?v=1.{}", p.slug.as_deref().unwrap(), p.state_v),
    };
    CardRef { variant, image }
}

/// Finishes a live page: card, ninja and receipt.
fn live(mut p: Page) -> Page {
    p.ninja = p.ninja_input().and_then(|i| ninja_state(i, false));
    if !matches!(p.kind, Kind::Unstored | Kind::Example) {
        let variant = Card::from_page(&p, false).expect("card").variant;
        p.card = Some(card_ref(&p, variant));
    }
    if p.state == DetState::Live && p.kind != Kind::Example {
        p.receipt = Some(RECEIPT.into());
    }
    p
}

/// A public page's quoted body: each passage with its context sentence.
fn public_excerpt(strong: bool) -> Text {
    let body = format!("{H1} {P1}\n\n{H2} {P2}");
    let (c1, p1, c2, p2) = (range(&body, H1), range(&body, P1), range(&body, H2), range(&body, P2));
    let anchor = |r: (u32, u32), role, from, to, frag: Option<&str>| Anchor {
        start: r.0,
        end: r.1,
        role,
        from_word: from,
        to_word: to,
        fragment: frag.map(Into::into),
    };
    if !strong {
        let body = H1.to_owned();
        let r = range(&body, H1);
        return Text {
            anchors: vec![anchor(
                r,
                AnchorRole::FirstSentence,
                1,
                words(H1),
                Some("We%20planted%20the%20tomatoes,second%20week%20of%20May."),
            )],
            body: Some(body),
            shown: Shown::Excerpt,
            further: Vec::new(),
            page_words: Some(612),
            withheld_reason: None,
        };
    }
    Text {
        anchors: vec![
            anchor(c1, AnchorRole::Context, 101, 119, None),
            anchor(
                p1,
                AnchorRole::Passage,
                120,
                150,
                Some("In%20today's%20fast-paced%20world,local%20food%20systems."),
            ),
            anchor(c2, AnchorRole::Context, 301, 318, None),
            anchor(p2, AnchorRole::Passage, 319, 347, Some("Moreover%2C%20by%20leveraging,keep%20growing%20together.")),
        ],
        body: Some(body),
        shown: Shown::Excerpt,
        further: vec![FurtherPassage {
            from_word: 480,
            to_word: 512,
            fragment: "Looking%20ahead,for%20generations%20to%20come.".into(),
            confidence: Some(93),
        }],
        page_words: Some(612),
        withheld_reason: None,
    }
}

fn withheld(reason: WithheldReason) -> Text {
    Text {
        body: None,
        shown: Shown::Withheld,
        anchors: Vec::new(),
        further: Vec::new(),
        page_words: Some(612),
        withheld_reason: Some(reason),
    }
}

fn full(body: &str) -> Text {
    Text {
        body: Some(body.into()),
        shown: Shown::Full,
        anchors: Vec::new(),
        further: Vec::new(),
        page_words: None,
        withheld_reason: None,
    }
}

fn public_strong(band: Band, tenths: u16, conf: &str, share: u8) -> Page {
    let mut p = base(Kind::PublicUrl, Some(PUBLIC), DetState::Live);
    let text = public_excerpt(true);
    let body = text.body.clone().unwrap();
    let mut run = banded_run(band, tenths, conf, share, 612, 612, CutKind::None);
    run.spans = vec![span(&body, P1, Some(97)), span(&body, P2, Some(92))];
    p.run = Some(run);
    p.text = Some(text);
    p.source = Some(source("garden-diary.example", Some("What the garden taught us this year")));
    live(p)
}

fn public_weak(band: Band, tenths: u16, conf: &str, share: u8) -> Page {
    let mut p = base(Kind::PublicUrl, Some(PUBLIC), DetState::Live);
    p.run = Some(banded_run(band, tenths, conf, share, 612, 612, CutKind::None));
    p.text = Some(public_excerpt(false));
    p.source = Some(source("garden-diary.example", Some("What the garden taught us this year")));
    live(p)
}

fn secret_body() -> String {
    format!("Tomato notes \u{1F345}\n\n{H1} {H3} {H4}\n\n{P1} {P2}\n\n{H5} {H2} {H6}")
}

/// A text over 1,000 words: the secret body, then paragraphs of the human
/// sentences. Returns the body, the words the free cut scores (whole
/// sentences, at most 1,000) and all its words.
fn long_body() -> (String, u32, u32) {
    let mut body = secret_body();
    let mut scored = words(&body);
    let mut over = false;
    for _ in 0..10 {
        body.push_str("\n\n");
        for (j, s) in [H1, H2, H3, H4, H5, H6].iter().enumerate() {
            if j > 0 {
                body.push(' ');
            }
            body.push_str(s);
            if !over && scored + words(s) <= 1000 {
                scored += words(s);
            } else {
                over = true;
            }
        }
    }
    let total = words(&body);
    (body, scored, total)
}

fn secret_strong() -> Page {
    let mut p = base(Kind::PrivateText, Some(SECRET), DetState::Live);
    let body = secret_body();
    let mut run = banded_run(Band::VeryStrong, 999, "above 99.9%", 86, words(&body), words(&body), CutKind::None);
    run.spans = vec![span(&body, &format!("{P1} {P2}"), Some(98))];
    p.run = Some(run);
    p.text = Some(full(&body));
    live(p)
}

fn short_body(strong: bool) -> String {
    if strong { format!("{P1} {P2}") } else { format!("{H1} {H2} {H3} We will try the beans again next spring.") }
}

fn secret_short(r: ShortResult) -> Page {
    let mut p = base(Kind::PrivateText, Some(SECRET), DetState::Live);
    let body = short_body(r == ShortResult::Strong);
    let mut run = short_run(r, words(&body));
    if r == ShortResult::Strong {
        run.spans = vec![span(&body, &body, None)];
    }
    p.run = Some(run);
    p.text = Some(full(&body));
    live(p)
}

fn pasted_post(rule: bool) -> Page {
    let mut p = secret_short(ShortResult::Strong);
    p.source = Some(Source { host: "x.com".into(), via: SourceVia::Submitter, checked: false, ..domain_only("x.com") });
    if rule {
        let run = p.run.as_mut().unwrap();
        run.spans.clear();
        run.spans_note = Some(SpansNote::Unavailable);
        p.text = Some(Text { page_words: None, ..withheld(WithheldReason::XSourceRule) });
    }
    p
}

fn paid_document() -> Page {
    let mut p = base(Kind::PrivatePaid, Some(SECRET2), DetState::Live);
    let body = format!("{H1} {H2}\n\n{P1} {P2}\n\n{H3}");
    let (s1, e1) = (0, range(&body, H2).1);
    let (s2, e2) = (range(&body, P1).0, range(&body, P2).1);
    let (s3, e3) = (range(&body, H3).0, utf16_len(&body));
    let section = |index, from, to, start, end, band, tenths: u16, conf: &str, share: u8, spans| Section {
        index,
        owned_from_word: from,
        owned_to_word: to,
        start,
        end,
        band,
        confidence_tenths: tenths,
        confidence_text: conf.into(),
        share_pct: share,
        share_text: format!("about {share}%"),
        spans_count: spans,
    };
    let mut run = Run {
        band: None,
        confidence_tenths: None,
        confidence_text: None,
        share_pct: None,
        share_text: None,
        length: None,
        spans_note: None,
        ..banded_run(Band::Strong, 0, "", 0, 3214, 3214, CutKind::None)
    };
    run.tokens = 4310;
    run.spans = vec![span(&body, &format!("{P1} {P2}"), Some(96))];
    p.run = Some(run);
    p.sections = Some(Sections {
        items: vec![
            section(1, 1, 987, s1, e1, Band::Weak, 214, "21.4%", 3, 0),
            section(2, 988, 1940, s2, e2, Band::VeryStrong, 999, "above 99.9%", 71, 1),
            section(3, 1941, 3214, s3, e3, Band::Unclear, 655, "65.5%", 12, 0),
        ],
        document: DocumentFigures {
            share_pct: 31,
            share_text: "about 31%".into(),
            strong_sections: 1,
            total_sections: 3,
        },
    });
    p.text = Some(full(&body));
    p.expires_at = Some(T0 + 30 * DAY);
    live(p)
}

/// Every page fixture, with what it shows.
fn pages() -> Vec<(&'static str, &'static str, Page)> {
    let mut v = Vec::new();
    v.push((
        "01-public-very-strong",
        "public_url, ranked, band 4",
        public_strong(Band::VeryStrong, 999, "above 99.9%", 86),
    ));
    v.push(("02-public-strong", "public_url, ranked, band 3", public_strong(Band::Strong, 973, "97.3%", 43)));
    v.push((
        "03-public-unclear",
        "band 2 public: title, first sentence, continue link",
        public_weak(Band::Unclear, 724, "72.4%", 20),
    ));
    v.push((
        "04-public-weak",
        "band 1 public: title, first sentence, continue link",
        public_weak(Band::Weak, 120, "12.0%", 4),
    ));
    let mut p = public_strong(Band::Strong, 963, "96.3%", 63);
    p.robots = Robots::Noindex;
    let s = p.source.as_mut().unwrap();
    s.reputation = Some(Reputation::Unranked);
    s.title = None;
    s.link = false;
    p.text = Some(withheld(WithheldReason::Unranked));
    p.run.as_mut().unwrap().spans.clear();
    v.push(("05-public-unranked", "public_url unranked: no title or excerpt, address unlinked", p));
    let mut p = public_strong(Band::Strong, 973, "97.3%", 43);
    p.robots = Robots::Noindex;
    p.source.as_mut().unwrap().status = Some(SourceStatus::Gone);
    p.text = Some(withheld(WithheldReason::SourceGone));
    p.run.as_mut().unwrap().spans.clear();
    v.push(("06-public-source-gone", "source gone: score and positions stay, excerpt goes", p));
    let mut p = public_strong(Band::Strong, 973, "97.3%", 43);
    p.robots = Robots::Noindex;
    p.source = Some(Source { status: Some(SourceStatus::Unlisted), ..domain_only("garden-diary.example") });
    p.text = Some(withheld(WithheldReason::Unlisted));
    p.run.as_mut().unwrap().spans.clear();
    v.push(("07-public-unlisted", "source unlisted: score and band, domain only", p));
    let mut p = public_strong(Band::VeryStrong, 999, "above 99.9%", 86);
    p.state = DetState::ExcerptWithheld;
    p.state_v = 2;
    p.robots = Robots::Noindex;
    p.text = Some(withheld(WithheldReason::LegalNotice));
    p.run.as_mut().unwrap().spans.clear();
    p.removal = Some(Removal {
        scope: RemovalScope::Excerpt,
        basis: "copyright".into(),
        jurisdiction: "US".into(),
        at: T0 + DAY,
    });
    p.receipt = None;
    p.ninja = None;
    p.card = Some(card_ref(&p, CardVariant::Generic));
    v.push(("08-public-excerpt-withheld", "excerpt_withheld: score, band, source link; generic card", p));
    let mut p = public_strong(Band::Strong, 973, "97.3%", 43);
    p.superseded_by = Some(NEWER.into());
    p.canonical = Some(NEWER.into());
    p.serving = serving(true);
    v.push(("09-public-superseded", "superseded_by: banner and canonical to the newest page", p));
    let mut p = public_strong(Band::VeryStrong, 999, "above 99.9%", 86);
    p.serving = serving(true);
    v.push(("10-public-older-package", "serving.differs_from_run: re-run banner", p));
    let mut p = base(Kind::PublicUrl, Some(PUBLIC), DetState::Pending);
    p.robots = Robots::Noindex;
    p.canonical = None;
    p.source = Some(domain_only("garden-diary.example"));
    p.pending = Some(Pending { lease_ended: true, job: None });
    v.push(("11-public-pending", "pending public: domain only, Run it", p));
    let mut p = base(Kind::PublicUrl, Some(PUBLIC), DetState::Failed);
    p.robots = Robots::Noindex;
    p.canonical = None;
    p.source = Some(domain_only("garden-diary.example"));
    p.failure = Some(Failure { code: RefusalCode::LoginWall });
    v.push(("12-public-failed", "failed: reason and domain only", p));
    let mut p = base(Kind::PublicUrl, Some(PUBLIC), DetState::Removed);
    p.state_v = 3;
    p.robots = Robots::Noindex;
    p.canonical = None;
    p.removal =
        Some(Removal { scope: RemovalScope::Page, basis: "copyright".into(), jurisdiction: "US".into(), at: T0 + DAY });
    v.push(("13-public-removed", "removed: 451 with blocked-by", p));
    let mut p = base(Kind::PublicUrl, Some(PUBLIC), DetState::Live);
    p.platform = Platform::Bluesky;
    p.robots = Robots::Noindex;
    let body = short_body(true);
    let mut run = short_run(ShortResult::Strong, words(&body));
    run.spans = vec![span(&body, &body, None)];
    p.run = Some(run);
    p.text = Some(Text { shown: Shown::Excerpt, ..full(&body) });
    p.source = Some(Source {
        via: SourceVia::Bluesky,
        url: Some("https://bsky.app/profile/gardener.example/post/3kq2abc".into()),
        title: None,
        site_name: None,
        ..source("bsky.app", None)
    });
    v.push(("14-bluesky-post", "public_url Bluesky post: short text, noindex", live(p)));
    v.push(("15-secret-text", "private_text: whole text, secret notice", secret_strong()));
    v.push((
        "16-secret-short-strong",
        "run.short strong: short gauge, ready, neutral shade",
        secret_short(ShortResult::Strong),
    ));
    v.push(("17-secret-short-none", "run.short none: short gauge, stoic", secret_short(ShortResult::None)));
    v.push(("18-secret-pasted-post", "pasted post: source given by the submitter, not checked", pasted_post(false)));
    v.push(("19-secret-x-source-rule", "x.com-source rule: result without the text", pasted_post(true)));
    let (body, scored, total) = long_body();
    let mut p = base(Kind::PrivateText, Some(SECRET), DetState::Live);
    let mut run = banded_run(Band::VeryStrong, 999, "above 99.9%", 86, scored, total, CutKind::FirstWords);
    run.spans = vec![span(&body, &format!("{P1} {P2}"), Some(98))];
    p.run = Some(run);
    p.text = Some(full(&body));
    p.serving = serving(true);
    let p = live(p);
    v.push(("20-secret-free-cut", "run.cut: first N of M words; re-run link (live private_text, older package)", p));
    let mut p = base(Kind::PrivateText, Some(SECRET), DetState::Deleted);
    p.state_v = 2;
    v.push(("21-secret-deleted", "deleted: 410", p));
    let mut p = base(Kind::PrivateText, Some(SECRET), DetState::Expired);
    p.expires_at = Some(T0 + DAY);
    v.push(("22-secret-expired", "expired: 410", p));
    let mut p = base(Kind::PrivateText, Some(SECRET), DetState::Disabled);
    p.state_v = 2;
    v.push(("23-secret-disabled", "disabled: 451 while a report is reviewed", p));
    v.push(("24-paid-document", "private_paid: section map, neutral dial, no ninja", paid_document()));
    let mut p = base(Kind::PrivatePaid, Some(SECRET2), DetState::Pending);
    p.pending = Some(Pending { lease_ended: false, job: Some(JobProgress { sections_done: 2, sections_total: 5 }) });
    v.push(("25-paid-pending", "pending paid job: section progress", p));
    let mut p = base(Kind::PrivatePaid, Some(SECRET2), DetState::Failed);
    p.failure = Some(Failure { code: RefusalCode::Scorer });
    v.push(("26-paid-failed", "failed paid job: nothing was charged", p));
    let mut p = base(Kind::Unstored, None, DetState::Live);
    let (body, scored, total) = long_body();
    let mut run = banded_run(Band::VeryStrong, 999, "above 99.9%", 86, scored, total, CutKind::FirstWords);
    run.spans = vec![span(&body, &format!("{P1} {P2}"), Some(98))];
    p.run = Some(run);
    p.text = Some(full(&body));
    v.push(("27-unstored", "unstored result: not saved, Get a link, free-cut notice", live(p)));
    let mut p = base(Kind::Example, None, DetState::Live);
    p.example_slot = Some(1);
    let text = public_excerpt(true);
    let body = text.body.clone().unwrap();
    let mut run = banded_run(Band::VeryStrong, 999, "above 99.9%", 86, 612, 612, CutKind::None);
    run.spans = vec![span(&body, P1, Some(97)), span(&body, P2, Some(92))];
    p.run = Some(run);
    p.text = Some(text);
    p.source =
        Some(Source { link: false, ..source("meridian-ledger.example", Some("Ledger notes: a synthetic example")) });
    v.push(("28-example", "kind example: synthetic example report, noindex", live(p)));
    v
}

/// Cards: variant × state, from page fixtures with their figures changed.
fn cards() -> Vec<(String, Card)> {
    let mut v = Vec::new();
    let states: [(Band, u16, &[u8]); 4] = [
        (Band::Weak, 450, &[4, 30]),
        (Band::Unclear, 724, &[20]),
        (Band::Strong, 973, &[4, 43, 80]),
        (Band::VeryStrong, 999, &[10, 63, 86]),
    ];
    for (band, tenths, shares) in states {
        for share in shares {
            let mut p = public_strong(band, tenths, "97.3%", *share);
            let run = p.run.as_mut().unwrap();
            run.confidence_text = Some(if band == Band::VeryStrong {
                "above 99.9%".into()
            } else {
                format!("{}.{}%", tenths / 10, tenths % 10)
            });
            if !band.is_strong() {
                run.spans.clear();
            }
            p.ninja = p.ninja_input().and_then(|i| ninja_state(i, false));
            let c = Card::from_page(&p, false).unwrap();
            v.push((format!("band-{}-{}", band.as_str(), c.ninja.unwrap().as_str()), c));
            if *share == shares[shares.len() - 1] {
                v.push((format!("generic-{}", band.as_str()), Card::from_page(&p, true).unwrap()));
            }
        }
    }
    let mut bsky = pages().into_iter().find(|x| x.0 == "14-bluesky-post").unwrap().2;
    v.push(("short-strong-bluesky".into(), Card::from_page(&bsky, false).unwrap()));
    bsky.run = Some(short_run(ShortResult::None, 40));
    bsky.ninja = Some(NinjaState::Stoic);
    v.push(("short-none-bluesky".into(), Card::from_page(&bsky, false).unwrap()));
    v.push(("secret-band-very_strong-strike".into(), Card::from_page(&secret_strong(), false).unwrap()));
    v.push(("secret-short-strong-ready".into(), Card::from_page(&secret_short(ShortResult::Strong), false).unwrap()));
    v.push(("secret-short-none-stoic".into(), Card::from_page(&secret_short(ShortResult::None), false).unwrap()));
    let free_cut = pages().into_iter().find(|x| x.0 == "20-secret-free-cut").unwrap().2;
    v.push(("secret-free-cut".into(), Card::from_page(&free_cut, false).unwrap()));
    v.push(("paid-document".into(), Card::from_page(&paid_document(), false).unwrap()));
    let withheld = pages().into_iter().find(|x| x.0 == "08-public-excerpt-withheld").unwrap().2;
    v.push(("generic-excerpt-withheld".into(), Card::from_page(&withheld, false).unwrap()));
    v
}

fn summary() -> Summary {
    Summary {
        kind: Kind::PrivateText,
        band: Some(Band::VeryStrong),
        short: None,
        strong_sections: None,
        total_sections: None,
        words: 150,
        created_at: T0,
        expires_at: None,
    }
}

fn id_h(b: u8) -> IdH {
    IdH([b; 32])
}

fn user(b: u8) -> UserId {
    UserId([b; 16])
}

// ---------------------------------------------------------------- the files

struct Entry {
    path: String,
    json: String,
}

fn pretty<T: Serialize>(v: &T) -> String {
    let mut s = serde_json::to_string_pretty(v).unwrap();
    s.push('\n');
    s
}

fn entry<T: Serialize + DeserializeOwned + PartialEq + Debug>(path: impl Into<String>, v: T) -> Entry {
    let json = pretty(&v);
    let back: T = serde_json::from_str(&json).unwrap();
    assert_eq!(back, v, "round trip");
    Entry { path: path.into(), json }
}

fn entries() -> Vec<Entry> {
    let mut e = Vec::new();
    let mut index = BTreeMap::new();
    for (name, row, page) in pages() {
        page.check().unwrap_or_else(|err| panic!("{name}: {err}"));
        index.insert(
            name.to_owned(),
            serde_json::json!({
                "shows": row,
                "status": page.http_status(),
                "robots": page.robots.header_value(),
                "canonical": page.canonical,
            }),
        );
        e.push(entry(format!("pages/{name}.json"), page));
    }
    e.push(Entry { path: "pages/INDEX.json".into(), json: pretty(&index) });
    for (name, card) in cards() {
        card.check().unwrap_or_else(|err| panic!("{name}: {err:?}"));
        e.push(entry(format!("cards/{name}.json"), card));
    }
    // Internal routes.
    e.push(entry("internal/page-request.json", PageRequest { slug: SECRET.into() }));
    e.push(entry(
        "internal/preview-request.json",
        PreviewRequest { text: secret_body(), seal: "AQ3vb2xkLXNlYWwtZXhhbXBsZQ".into() },
    ));
    e.push(entry("internal/card-request.json", CardRequest { slug: PUBLIC.into() }));
    e.push(entry("internal/states-request.json", StatesRequest { id_h: vec![id_h(1), id_h(2), id_h(3)] }));
    e.push(entry(
        "internal/states-answer.json",
        StatesAnswer {
            states: vec![
                StateEntry { id_h: id_h(1), state: Some(DetState::Live), expires_at: None },
                StateEntry { id_h: id_h(2), state: Some(DetState::Expired), expires_at: Some(T0 + DAY) },
                StateEntry { id_h: id_h(3), state: None, expires_at: None },
            ],
        },
    ));
    e.push(entry(
        "internal/owner-peek-request.json",
        OwnerRequest {
            action: OwnerAction::Peek,
            slug: SECRET.into(),
            manage: Some("4kq8mz2p7wx3rt9vn6bc1hj5dy".into()),
        },
    ));
    e.push(entry(
        "internal/owner-claim-request.json",
        OwnerRequest {
            action: OwnerAction::Claim,
            slug: SECRET.into(),
            manage: Some("4kq8mz2p7wx3rt9vn6bc1hj5dy".into()),
        },
    ));
    e.push(entry(
        "internal/owner-delete-request.json",
        OwnerRequest { action: OwnerAction::Delete, slug: SECRET.into(), manage: None },
    ));
    e.push(entry(
        "internal/owner-release-request.json",
        OwnerRequest { action: OwnerAction::Release, slug: SECRET.into(), manage: None },
    ));
    e.push(entry(
        "internal/peek-answer.json",
        PeekAnswer { id_h: id_h(1), summary: summary(), manage_ok: true, owner: OwnerRelation::None },
    ));
    e.push(entry(
        "internal/claim-answer.json",
        ClaimAnswer { id_h: id_h(1), summary: summary(), is_owner: true, newly_owned: true },
    ));
    e.push(entry(
        "internal/claim-answer-bookmark.json",
        ClaimAnswer { id_h: id_h(1), summary: summary(), is_owner: false, newly_owned: false },
    ));
    e.push(entry("internal/owner-done.json", OwnerDone { ok: true, code: None }));
    e.push(entry("internal/owner-done-not-owner.json", OwnerDone { ok: false, code: Some(RefusalCode::NotOwner) }));
    e.push(entry("internal/examples-answer-empty.json", ExamplesAnswer { examples: Vec::new() }));
    let example = pages().into_iter().find(|x| x.0 == "28-example").unwrap().2;
    e.push(entry(
        "internal/examples-answer.json",
        ExamplesAnswer { examples: vec![ExampleEntry { slot: 1, source: ExampleSource::Synthetic, page: example }] },
    ));
    e.push(entry("internal/example-request.json", ExampleRequest { slot: 1 }));
    e.push(entry(
        "internal/removals-answer.json",
        RemovalsAnswer {
            entries: vec![RemovalEntry {
                at: T0 + DAY,
                public_slug: PUBLIC.into(),
                scope: RemovalScope::Page,
                basis: "copyright".into(),
                jurisdiction: "US".into(),
            }],
            secret_monthly: vec![MonthlyCount { month: "2026-11".into(), count: 2 }],
        },
    ));
    // Forwarded routes.
    e.push(entry(
        "api/share-request.json",
        ShareRequest {
            text: secret_body(),
            seal: "AQ3vb2xkLXNlYWwtZXhhbXBsZQ".into(),
            expiry: Expiry::Keep,
            save: true,
            source: Some("https://x.com/gardener/status/1".into()),
        },
    ));
    e.push(entry(
        "api/share-answer.json",
        ShareAnswer { slug: SECRET.into(), manage: "4kq8mz2p7wx3rt9vn6bc1hj5dy".into(), saved: true, reason: None },
    ));
    e.push(entry(
        "api/share-answer-unsaved.json",
        ShareAnswer {
            slug: SECRET.into(),
            manage: "4kq8mz2p7wx3rt9vn6bc1hj5dy".into(),
            saved: false,
            reason: Some(SaveReason::AccountUnavailable),
        },
    ));
    e.push(entry("api/rerun-request.json", RerunRequest { slug: SECRET.into() }));
    e.push(entry(
        "api/rerun-answer.json",
        RerunAnswer { slug: SECRET2.into(), manage: Some("9xw3kq8mz2p7rt4vn6bc1hj5dy".into()) },
    ));
    e.push(entry("api/state-request.json", StateRequest { slug: PUBLIC.into() }));
    e.push(entry("api/state-answer.json", StateAnswer { state: DetState::Pending, lease_ended: true, code: None }));
    e.push(entry(
        "api/manage-request.json",
        ManageRequest {
            slug: SECRET2.into(),
            manage: "9xw3kq8mz2p7rt4vn6bc1hj5dy".into(),
            action: ManageAction::SetExpiry,
            expiry: Some(Expiry::Week),
        },
    ));
    e.push(entry(
        "api/manage-answer.json",
        ManageAnswer {
            ok: true,
            state: DetState::Live,
            expires_at: Some(T0 + 7 * DAY),
            latest_expiry: Some(T0 + 30 * DAY),
            choices: vec![Expiry::Day, Expiry::Week, Expiry::Month],
        },
    ));
    e.push(entry(
        "api/quote-request.json",
        QuoteRequest { text: secret_body(), seal: "AQ3vb2xkLXNlYWwtZXhhbXBsZQ".into() },
    ));
    e.push(entry(
        "api/quote-answer.json",
        QuoteAnswer {
            sections: 5,
            words: 3214,
            free_words: 987,
            paid_tokens: 3020,
            draw_plan: 2000,
            draw_credit: 1020,
            enough: true,
            quote_seal: "AQ3xdW90ZS1zZWFsLWV4YW1wbGU".into(),
        },
    ));
    e.push(entry(
        "api/start-request.json",
        StartRequest {
            text: secret_body(),
            seal: "AQ3vb2xkLXNlYWwtZXhhbXBsZQ".into(),
            quote_seal: "AQ3xdW90ZS1zZWFsLWV4YW1wbGU".into(),
            expiry: Expiry::Keep,
        },
    ));
    e.push(entry(
        "api/start-answer.json",
        StartAnswer { slug: SECRET2.into(), manage: "9xw3kq8mz2p7rt4vn6bc1hj5dy".into() },
    ));
    e.push(entry("api/job-request.json", JobRequest { slug: SECRET2.into() }));
    e.push(entry("api/job-line-running.json", JobLine::Running { section: 3, of: 5 }));
    e.push(entry("api/job-line-progress.json", JobLine::Progress { section: 3, of: 5 }));
    e.push(entry("api/job-line-done.json", JobLine::Done));
    e.push(entry("api/job-line-failed.json", JobLine::Failed { code: RefusalCode::Scorer }));
    let mut fields = BTreeMap::new();
    fields.insert("name".to_owned(), "A. Writer".to_owned());
    fields.insert("work".to_owned(), "My essay on community gardens".to_owned());
    fields.insert("good_faith".to_owned(), "yes".to_owned());
    e.push(entry(
        "api/removal-request.json",
        RemovalRequest { step: RemovalStep::Review, track: CaseKind::Copyright, target: PUBLIC.into(), fields },
    ));
    e.push(entry(
        "api/removal-review-answer.json",
        RemovalReviewAnswer { complete: false, missing: vec!["signature".into()] },
    ));
    e.push(entry(
        "api/removal-received.json",
        RemovalReceived {
            case_id: "td-8k2m4q7x".into(),
            status_token: "status-fixture".into(),
            auto: Some(AutoStep::ExcerptWithheld),
        },
    ));
    e.push(entry("api/removal-status-request.json", RemovalStatusRequest { token: "status-fixture".into() }));
    e.push(entry(
        "api/removal-status-answer.json",
        RemovalStatusAnswer {
            case_id: "td-8k2m4q7x".into(),
            state: CaseState::Reviewing,
            received_at: T0,
            updated_at: T0 + 30 * MIN,
            due_at: Some(T0 + 14 * DAY),
        },
    ));
    // Refusals: one per code.
    for code in RefusalCode::ALL {
        let mut r = ApiRefusal::new(*code);
        if *code == RefusalCode::TooShort {
            r.words = Some(17);
            r.tokens = Some(25);
            r.min_words = Some(24);
            r.total_words = Some(17);
            r.cut = Some(CutKind::None);
        }
        e.push(entry(format!("refusals/{}.json", code.as_str()), r));
    }
    // Queue messages.
    let q = [
        (
            "purge",
            Job::Purge {
                urls: vec![
                    format!("https://site.example/d/{PUBLIC}"),
                    format!("https://site.example/d/{PUBLIC}/card.png"),
                ],
            },
        ),
        ("owner-purge", Job::OwnerPurge { user_id: user(7), mode: PurgeMode::Delete }),
        ("commit-hold", Job::CommitHold { det_id_h: id_h(4) }),
        ("legal-notice", Job::LegalNotice { case_id: "td-8k2m4q7x".into(), kind: CaseKind::Copyright }),
        ("sitemap-dirty", Job::SitemapDirty { shard: 3 }),
        ("indexnow", Job::Indexnow { public_slug: PUBLIC.into(), change: IndexChange::Updated }),
        ("rerun-op", Job::RerunOp { source_id: 42 }),
        ("owner-rekey", Job::OwnerRekey { from_user_id: user(7), to_user_id: user(8) }),
    ];
    for (name, job) in q {
        e.push(entry(format!("queue/{name}.json"), QueueMessage::new(job)));
    }
    e
}

fn dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
}

/// The directories this test owns (`account/` belongs to the account-wire
/// crate's test).
const OWNED: &[&str] = &["pages", "cards", "internal", "api", "refusals", "queue"];

#[test]
fn fixtures_match() {
    let bless = std::env::var_os("SLOP_NINJA_BLESS").is_some();
    let all = entries();
    let mut seen = BTreeSet::new();
    for e in &all {
        assert!(seen.insert(e.path.clone()), "duplicate {}", e.path);
        let p = dir().join(&e.path);
        if bless {
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(&p, &e.json).unwrap();
        } else {
            let on_disk = std::fs::read_to_string(&p).unwrap_or_else(|_| panic!("missing {}", e.path));
            assert_eq!(on_disk, e.json, "{} differs (SLOP_NINJA_BLESS=1 rewrites it)", e.path);
        }
    }
    // No stray file in an owned directory.
    for d in OWNED {
        for f in std::fs::read_dir(dir().join(d)).unwrap() {
            let name = format!("{d}/{}", f.unwrap().file_name().to_string_lossy());
            assert!(seen.contains(&name), "stray fixture {name}");
        }
    }
}

#[test]
fn files_round_trip_through_their_types() {
    fn check<T: Serialize + DeserializeOwned>(path: &Path) {
        let s = std::fs::read_to_string(path).unwrap();
        let v: T = serde_json::from_str(&s).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
        assert_eq!(pretty(&v), s, "{}", path.display());
    }
    for f in std::fs::read_dir(dir().join("pages")).unwrap() {
        let p = f.unwrap().path();
        if p.file_name().unwrap() != "INDEX.json" {
            check::<Page>(&p);
            let page: Page = serde_json::from_str(&std::fs::read_to_string(&p).unwrap()).unwrap();
            page.check().unwrap_or_else(|e| panic!("{}: {e}", p.display()));
        }
    }
    for f in std::fs::read_dir(dir().join("cards")).unwrap() {
        check::<Card>(&f.unwrap().path());
    }
    for f in std::fs::read_dir(dir().join("refusals")).unwrap() {
        check::<ApiRefusal>(&f.unwrap().path());
    }
    for f in std::fs::read_dir(dir().join("queue")).unwrap() {
        check::<QueueMessage>(&f.unwrap().path());
    }
}

#[test]
fn the_page_set_covers_every_plan_step() {
    let pages = pages();
    let has = |f: &dyn Fn(&Page) -> bool| pages.iter().any(|(_, _, p)| f(p));
    for s in DetState::ALL {
        assert!(has(&|p| p.state == *s), "state {s}");
    }
    for k in Kind::ALL {
        assert!(has(&|p| p.kind == *k), "kind {k}");
    }
    for b in Band::ALL {
        assert!(
            has(&|p| p.kind == Kind::PublicUrl && p.run.as_ref().is_some_and(|r| r.band == Some(*b))),
            "public band {b}"
        );
    }
    for r in ShortResult::ALL {
        assert!(has(&|p| p.run.as_ref().is_some_and(|x| x.short == Some(*r))), "short {r}");
    }
    for w in WithheldReason::ALL {
        assert!(has(&|p| p.text.as_ref().is_some_and(|t| t.withheld_reason == Some(*w))), "withheld {w}");
    }
    for n in NinjaState::ALL {
        assert!(has(&|p| p.ninja == Some(*n)), "ninja {n}");
    }
    assert!(has(&|p| p.superseded_by.is_some()));
    assert!(has(&|p| p.serving.differs_from_run && p.kind == Kind::PrivateText));
    assert!(has(&|p| p.run.as_ref().is_some_and(|r| r.cut != CutKind::None)));
    assert!(has(&|p| p.source.as_ref().is_some_and(|s| s.via == SourceVia::Submitter)));
    assert!(has(&|p| p.source.as_ref().is_some_and(|s| s.reputation == Some(Reputation::Unranked))));
    assert!(has(&|p| p.platform == Platform::Bluesky));
    assert!(has(&|p| p
        .text
        .as_ref()
        .and_then(|t| t.body.as_deref())
        .is_some_and(|b| b.chars().any(|c| c.len_utf16() == 2))));
    let cards = cards();
    for v in CardVariant::ALL {
        assert!(cards.iter().any(|(_, c)| c.variant == *v), "card variant {v}");
    }
    for n in NinjaState::ALL {
        assert!(cards.iter().any(|(_, c)| c.variant == CardVariant::Band && c.ninja == Some(*n)), "card ninja {n}");
    }
}
