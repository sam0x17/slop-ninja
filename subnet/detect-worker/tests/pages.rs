//! Page JSON per state under the display policy: the store's builders,
//! given the rows and documents a contract fixture stands for, produce that
//! fixture exactly (`subnet/crates/detect-page/tests/fixtures/pages/`).

use slop_ninja_detect_page::codes::{Adapter, DetState, Kind, Platform, Reputation, SourceStatus};
use slop_ninja_detect_page::page::{JobProgress, Page, Run};
use slop_ninja_detect_page::refusal::RefusalCode;
use slop_ninja_detect_worker::display::{PageSwitches, SourceRow};
use slop_ninja_detect_worker::doc::{DOC_V, RunDoc, SecretDoc, Serving, SourceCheck, StoredResult};
use slop_ninja_detect_worker::page::{PageCtx, PublicExtras, public_page, secret_page, unstored_page};
use slop_ninja_detect_worker::store::DetRow;

fn fixture(name: &str) -> Page {
    let p = format!("{}/../crates/detect-page/tests/fixtures/pages/{name}.json", env!("CARGO_MANIFEST_DIR"));
    serde_json::from_str(&std::fs::read_to_string(p).unwrap()).unwrap()
}

const SERVING_LOAD: &str = "load-serving";

fn serving(p: &Page) -> Serving {
    let digest = p
        .run
        .as_ref()
        .map(|r| r.package.published_digest.clone())
        .unwrap_or_else(|| "e2536d12dff593d7b11895b3cbf6b10232601abfbebde248325afd707722b5e8".into());
    assert!(digest.starts_with(&p.serving.package_pub_short));
    Serving {
        load: SERVING_LOAD.into(),
        label: p.serving.package_label.clone(),
        published: digest,
        pending: p.serving.publication_pending,
        scorer_build: "7030a86f".into(),
        evidence: "evidence-d24-web-v1".into(),
    }
}

fn run_load(p: &Page) -> &'static str {
    if p.serving.differs_from_run { "load-older" } else { SERVING_LOAD }
}

fn stored(run: &Run, load: &str) -> StoredResult {
    StoredResult {
        scored_at: run.scored_at,
        package_load: load.to_owned(),
        package_label: run.package.label.clone(),
        package_pub: run.package.published_digest.clone(),
        publication_pending: run.package.publication_pending,
        scorer_build: run.scorer_build.clone(),
        policy: run.policy.clone(),
        evidence: run.evidence.clone(),
        band: run.band,
        short: run.short,
        confidence_tenths: run.confidence_tenths,
        confidence_text: run.confidence_text.clone(),
        share_pct: run.share_pct,
        share_text: run.share_text.clone(),
        words: run.words,
        total_words: run.total_words,
        cut: run.cut,
        tokens: run.tokens,
        length: run.length,
        spans: run.spans.clone(),
        spans_note: run.spans_note,
        markers: run.markers.clone(),
        invisible: run.invisible,
        raw: None,
    }
}

fn row(p: &Page) -> DetRow {
    DetRow {
        id_h: [1; 32],
        pepper_v: p.kind.is_secret().then_some(1),
        public_slug: (p.kind == Kind::PublicUrl).then(|| p.slug.clone().unwrap()),
        kind: p.kind,
        platform: p.platform,
        state: p.state,
        state_v: p.state_v,
        refusal: p.ended.map(|e| e.code).or(p.failure.map(|f| f.code)),
        created_at: p.created_at.unwrap(),
        updated_at: p.created_at.unwrap(),
        expires_at: p.expires_at,
        source_id: Some(1),
        run_id: Some(1),
        superseded_by: p.superseded_by.as_ref().map(|_| [2; 32]),
        superseded_slug: p.superseded_by.clone(),
        package_load: Some(run_load(p).to_owned()),
        manage_h: None,
        owner_h: None,
        owner_via: None,
        doc_key: None,
        index_force: None,
        copy_of: None,
        expiry_cap: None,
    }
}

fn ctx<'a>(p: &Page, s: &'a Serving, sw: PageSwitches) -> PageCtx<'a> {
    // An expired fixture is read after its expiry.
    let now = p.expires_at.map_or(p.created_at.unwrap_or(0) + 1, |e| e + 1);
    let now = if p.state == DetState::Expired { now } else { p.created_at.unwrap_or(0) + 1 };
    PageCtx { serving: s, switches: sw, now, receipt: p.receipt.clone() }
}

fn assert_page(name: &str, got: Result<Page, RefusalCode>, want: &Page) {
    let got = got.unwrap_or_else(|e| panic!("{name}: {e}"));
    if &got != want {
        panic!(
            "{name}:\n got {}\nwant {}",
            serde_json::to_string_pretty(&got).unwrap(),
            serde_json::to_string_pretty(want).unwrap()
        );
    }
}

#[test]
fn secret_pages_match_their_fixtures() {
    for name in [
        "15-secret-text",
        "16-secret-short-strong",
        "17-secret-short-none",
        "18-secret-pasted-post",
        "19-secret-pasted-x-source",
        "20-secret-free-cut",
        "21-secret-deleted",
        "22-secret-expired",
        "23-secret-disabled",
        "24-paid-document",
        "25-paid-pending",
        "26-paid-failed",
    ] {
        let want = fixture(name);
        let s = serving(&want);
        let doc = want.run.as_ref().map(|run| SecretDoc {
            v: DOC_V,
            text: want.text.as_ref().unwrap().body.clone().unwrap(),
            result_full: stored(run, run_load(&want)),
            source_check: want.source.as_ref().map(|s| SourceCheck { host: s.host.clone(), checked: s.checked }),
            sections: want.sections.clone(),
        });
        let job = want.pending.as_ref().and_then(|p| p.job);
        let got = secret_page(
            want.slug.as_deref().unwrap(),
            &row(&want),
            doc.as_ref(),
            job,
            &ctx(&want, &s, PageSwitches::DEFAULT),
        );
        assert_page(name, got, &want);
    }
}

fn source_row(p: &Page, adapter: Adapter, status: SourceStatus, reputation: Reputation) -> SourceRow {
    let s = p.source.as_ref();
    SourceRow {
        source_id: 1,
        canon_url: s.and_then(|s| s.url.clone()),
        host: s.map_or("x.com".into(), |s| s.host.clone()),
        adapter,
        title: s.and_then(|s| s.title.clone()),
        site_name: s.and_then(|s| s.site_name.clone()),
        author: s.and_then(|s| s.author.clone()),
        published_at: s.and_then(|s| s.published_at),
        fetched_at: s.and_then(|s| s.fetched_at),
        fresh_until: None,
        state: status,
        reputation,
    }
}

/// The run document of a fixture that shows its text.
fn run_doc(p: &Page, result_spans_from: Option<&Page>) -> RunDoc {
    let base = result_spans_from.unwrap_or(p);
    let t = base.text.as_ref().unwrap();
    let mut result = stored(p.run.as_ref().unwrap(), run_load(p));
    result.spans = base.run.as_ref().unwrap().spans.clone();
    RunDoc {
        v: DOC_V,
        body: t.body.clone().unwrap_or_default(),
        anchors: t.anchors.clone(),
        further: t.further.clone(),
        page_words: t.page_words.unwrap_or(0),
        page_hash: p.source.as_ref().and_then(|s| s.page_hash.clone()),
        result_full: result,
    }
}

/// The source row and run document a public fixture stands for.
type Inputs<'a> = Box<dyn Fn(&Page) -> (Option<SourceRow>, Option<RunDoc>) + 'a>;

#[test]
fn public_pages_match_their_fixtures() {
    let ranked_ok = |p: &Page| source_row(p, Adapter::Web, SourceStatus::Ok, Reputation::Ranked);
    let cases: Vec<(&str, Inputs<'_>)> = vec![
        ("01-public-very-strong", Box::new(|p| (Some(ranked_ok(p)), Some(run_doc(p, None))))),
        ("02-public-strong", Box::new(|p| (Some(ranked_ok(p)), Some(run_doc(p, None))))),
        ("03-public-unclear", Box::new(|p| (Some(ranked_ok(p)), Some(run_doc(p, None))))),
        ("04-public-weak", Box::new(|p| (Some(ranked_ok(p)), Some(run_doc(p, None))))),
        (
            "05-public-unranked",
            Box::new(|p| {
                let mut s = source_row(p, Adapter::Web, SourceStatus::Ok, Reputation::Unranked);
                s.title = Some("A title the page must not show".into());
                (Some(s), Some(run_doc(p, Some(&fixture("01-public-very-strong")))))
            }),
        ),
        (
            "06-public-source-gone",
            Box::new(|p| {
                (
                    Some(source_row(p, Adapter::Web, SourceStatus::Gone, Reputation::Ranked)),
                    Some(run_doc(p, Some(&fixture("01-public-very-strong")))),
                )
            }),
        ),
        (
            "07-public-unlisted",
            Box::new(|p| {
                let mut s = source_row(p, Adapter::Web, SourceStatus::Unlisted, Reputation::Ranked);
                s.canon_url = Some("https://garden-diary.example/2026/11/garden-diary".into());
                s.title = Some("hidden".into());
                (Some(s), Some(run_doc(p, Some(&fixture("01-public-very-strong")))))
            }),
        ),
        (
            "08-public-excerpt-withheld",
            Box::new(|p| (Some(ranked_ok(p)), Some(run_doc(p, Some(&fixture("01-public-very-strong")))))),
        ),
        ("09-public-superseded", Box::new(|p| (Some(ranked_ok(p)), Some(run_doc(p, None))))),
        ("10-public-older-package", Box::new(|p| (Some(ranked_ok(p)), Some(run_doc(p, None))))),
        ("11-public-pending", Box::new(|p| (Some(ranked_ok(p)), None))),
        ("12-public-failed", Box::new(|p| (Some(ranked_ok(p)), None))),
        ("13-public-removed", Box::new(|_| (None, None))),
        (
            "14-bluesky-post",
            Box::new(|p| {
                (Some(source_row(p, Adapter::Bluesky, SourceStatus::Ok, Reputation::Ranked)), Some(run_doc(p, None)))
            }),
        ),
        (
            "29-x-post-short",
            Box::new(|p| {
                let mut s = source_row(p, Adapter::X, SourceStatus::Ok, Reputation::Ranked);
                s.canon_url = Some(p.x.as_ref().unwrap().post_url.to_string());
                let mut result = stored(p.run.as_ref().unwrap(), run_load(p));
                result.spans_note = None;
                let doc = RunDoc {
                    v: DOC_V,
                    body: String::new(),
                    anchors: vec![],
                    further: vec![],
                    page_words: 0,
                    page_hash: None,
                    result_full: result,
                };
                (Some(s), Some(doc))
            }),
        ),
        ("32-x-post-removed", Box::new(|_| (None, None))),
        ("33-x-post-gone", Box::new(|_| (None, None))),
        (
            "36-x-post-pending",
            Box::new(|p| (Some(source_row(p, Adapter::X, SourceStatus::Ok, Reputation::Ranked)), None)),
        ),
    ];
    for (name, inputs) in cases {
        let want = fixture(name);
        let s = serving(&want);
        let (source, doc) = inputs(&want);
        let ex = PublicExtras {
            source,
            supersedes: want.supersedes.clone(),
            removal: want.removal.clone(),
            lease_ended: want.pending.as_ref().is_some_and(|p| p.lease_ended),
            host_opted_out: false,
        };
        let generic =
            want.card.as_ref().is_some_and(|c| c.variant.as_str() == "generic") && want.state == DetState::Live;
        let sw = PageSwitches { cards_generic: generic, ..PageSwitches::DEFAULT };
        let got = public_page(want.slug.as_deref().unwrap(), &row(&want), &ex, doc.as_ref(), &ctx(&want, &s, sw));
        assert_page(name, got, &want);
    }
}

#[test]
fn every_x_page_ends_under_a_purge() {
    let live = fixture("29-x-post-short");
    let want = fixture("35-x-post-purged");
    let s = serving(&live);
    let src = SourceRow {
        source_id: 1,
        canon_url: Some("https://x.com/i/status/2".into()),
        host: "x.com".into(),
        adapter: Adapter::X,
        title: None,
        site_name: None,
        author: None,
        published_at: None,
        fetched_at: Some(1),
        fresh_until: None,
        state: SourceStatus::Ok,
        reputation: Reputation::Ranked,
    };
    let ex = PublicExtras { source: Some(src), ..PublicExtras::default() };
    let sw = PageSwitches { x_purged: true, ..PageSwitches::DEFAULT };
    let mut r = row(&live);
    r.state_v = want.state_v;
    let got = public_page("x4m8q2t", &r, &ex, None, &ctx(&want, &s, sw)).unwrap();
    assert_eq!((got.state, got.http_status(), got.ended), (DetState::Deleted, 410, want.ended));
    assert!(got.run.is_none() && got.x.is_none() && got.source.is_none());
}

#[test]
fn a_live_page_without_its_document_is_temporarily_unavailable() {
    for name in ["29-x-post-short", "01-public-very-strong"] {
        let want = fixture(name);
        let s = serving(&want);
        let adapter = if want.platform == Platform::X { Adapter::X } else { Adapter::Web };
        let ex = PublicExtras {
            source: Some(source_row(&want, adapter, SourceStatus::Ok, Reputation::Ranked)),
            ..PublicExtras::default()
        };
        let got =
            public_page(want.slug.as_deref().unwrap(), &row(&want), &ex, None, &ctx(&want, &s, PageSwitches::DEFAULT));
        assert_eq!(got, Err(RefusalCode::ReportUnavailable), "{name}");
        assert_eq!(RefusalCode::ReportUnavailable.status(), 503);
    }
    let want = fixture("15-secret-text");
    let s = serving(&want);
    let got =
        secret_page(want.slug.as_deref().unwrap(), &row(&want), None, None, &ctx(&want, &s, PageSwitches::DEFAULT));
    assert_eq!(got, Err(RefusalCode::ReportUnavailable));
    let pending = fixture("25-paid-pending");
    let got = secret_page(
        pending.slug.as_deref().unwrap(),
        &row(&pending),
        None,
        None::<JobProgress>,
        &ctx(&pending, &s, PageSwitches::DEFAULT),
    );
    assert_eq!(got, Err(RefusalCode::ReportUnavailable), "a pending paid page without its job");
}

#[test]
fn the_switches_narrow_pages() {
    let want = fixture("01-public-very-strong");
    let s = serving(&want);
    let src = source_row(&want, Adapter::Web, SourceStatus::Ok, Reputation::Ranked);
    let ex = PublicExtras { source: Some(src), ..PublicExtras::default() };
    let doc = run_doc(&want, None);
    let sw = PageSwitches { cards_generic: true, index: false, excerpts: false, ..PageSwitches::DEFAULT };
    let got = public_page("k7m2xq9", &row(&want), &ex, Some(&doc), &ctx(&want, &s, sw)).unwrap();
    assert_eq!(got.robots.as_str(), "noindex");
    assert!(got.ninja.is_none());
    assert_eq!(got.card.as_ref().unwrap().variant.as_str(), "generic");
    let t = got.text.as_ref().unwrap();
    assert!(t.body.is_none() && t.anchors.is_empty());
    assert_eq!(t.further.len(), 3, "positions only");
    assert!(got.run.as_ref().unwrap().spans.is_empty());
}

#[test]
fn the_unstored_result_matches_its_fixtures() {
    for (name, open) in [("27-unstored", false), ("39-unstored-paid-open", true)] {
        let want = fixture(name);
        let s = serving(&want);
        let r = stored(want.run.as_ref().unwrap(), SERVING_LOAD);
        let text = want.text.as_ref().unwrap().body.clone().unwrap();
        let c = PageCtx { serving: &s, switches: PageSwitches::DEFAULT, now: 1, receipt: want.receipt.clone() };
        assert_page(name, unstored_page(&text, &r, open, &c), &want);
    }
}
