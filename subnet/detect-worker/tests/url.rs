//! The public URL check end to end over the store's own migration (foreign
//! keys on), with counting stubs for the fetch Worker, `DetectHost` and the
//! scorer.

mod common;
mod stubs;

use common::{Harness, block_on};
use slop_ninja_detect_fetch_wire::{Outcome, XPostAnswer};
use slop_ninja_detect_page::codes::{DetState, Platform, Robots};
use slop_ninja_detect_page::origins::Origins;
use slop_ninja_detect_page::queue::Job;
use slop_ninja_detect_page::refusal::RefusalCode;
use slop_ninja_detect_url::Policy;
use slop_ninja_detect_worker::doc::{RunDoc, Serving};
use slop_ninja_detect_worker::keys::{Pepper, Peppers, StoreKeys};
use slop_ninja_detect_worker::share::Keys;
use slop_ninja_detect_worker::url_flow::{Admitted, Ended, UrlCtx, admit, drive};
use slop_ninja_detect_worker::{reads, sitemap};
use stubs::*;

struct World {
    h: Harness,
    fetch: FetchStub,
    host: HostStub,
    scorer: ScorerStub,
    store: StoreKeys,
    peppers: Peppers,
    origins: Origins,
    serving: Serving,
    policy: Policy,
}

impl World {
    fn new() -> World {
        let h = Harness::new();
        for (k, v) in [
            ("DETECT_TOOL", "on"),
            ("DETECT_URLS", "on"),
            ("DETECT_X", "on"),
            ("DETECT_PAGES", "on"),
            ("DETECT_INDEX", "on"),
            ("DETECT_EXCERPTS", "on"),
        ] {
            h.vars.borrow_mut().insert(k.into(), v.into());
        }
        let host = HostStub::default();
        host.ranked.borrow_mut().insert("example.com".into());
        World {
            h,
            fetch: FetchStub::default(),
            host,
            scorer: ScorerStub::default(),
            store: StoreKeys::derive(&[7u8; 32]),
            peppers: Peppers::new(vec![Pepper::new(1, [3u8; 32])]).unwrap(),
            origins: Origins::from_vars("https://site.example", "https://s.site.example").unwrap(),
            serving: serving(),
            policy: Policy::from_origins(Some("https://site.example"), Some("https://s.site.example")),
        }
    }

    fn ctx(&self) -> UrlCtx<'_> {
        UrlCtx {
            serving: &self.serving,
            origins: &self.origins,
            keys: &self.store,
            policy: &self.policy,
            fetch_ms: 10_000,
            lease_ms: 120_000,
            html_bound: 4_000_000,
        }
    }

    fn admit(&self, url: &str) -> Result<Admitted, RefusalCode> {
        block_on(admit(&self.h, &self.host, &self.ctx(), url)).map_err(|r| r.code)
    }

    /// Admits and drives to the end, as one submitter does.
    fn check(&self, url: &str) -> Result<Ended, RefusalCode> {
        match self.admit(url)? {
            Admitted::Done(slug) | Admitted::Joined(slug) => Ok(Ended { slug, failed: None }),
            Admitted::Drive(d) => Ok(block_on(drive(&self.h, &self.host, &self.fetch, &self.scorer, &self.ctx(), &d))),
        }
    }

    fn page(&self, slug: &str) -> Result<slop_ninja_detect_page::page::Page, RefusalCode> {
        block_on(reads::page(&self.h, Keys { store: &self.store, peppers: &self.peppers }, &self.serving, slug))
    }

    fn rows(&self, table: &str) -> i64 {
        self.h.count(&format!("SELECT count(*) FROM {table}"))
    }
}

const URL: &str = "https://example.com/garden/notes";

#[test]
fn a_page_is_checked_once_and_its_link_reused() {
    let w = World::new();
    w.fetch.page(URL, &article(14));
    let e = w.check(URL).unwrap();
    assert_eq!(e.failed, None);
    let p = w.page(&e.slug).unwrap();
    assert_eq!((p.state, p.platform), (DetState::Live, Platform::Web));
    // A ranked long-form page with its full analysis passes every gate.
    assert_eq!(p.robots, Robots::Index);
    let text = p.text.as_ref().unwrap();
    let body = text.body.as_deref().unwrap();
    assert!(body.contains(AI2) && body.split_whitespace().count() <= article(14).split_whitespace().count() / 4);
    assert_eq!(p.source.as_ref().unwrap().host, "example.com");
    // The run document holds no full text.
    let run_id: i64 =
        w.h.db.query_row("SELECT run_id FROM detections WHERE public_slug = ?1", [&e.slug], |r| r.get(0)).unwrap();
    let doc = RunDoc::decode(&w.h.doc(&format!("r/{run_id}")).unwrap()).unwrap();
    assert!(doc.body.len() < article(14).len() / 2);
    assert!(w.h.dump().find(&article(14)).is_none());
    // The same link, and a tracking-tagged form of it, answer the page with
    // no fetch and no scan.
    let (f, s) = (w.fetch.fetch_calls.get(), w.scorer.calls.get());
    for u in [URL, "https://EXAMPLE.com/garden/notes?utm_source=x#frag"] {
        assert_eq!(w.admit(u).unwrap(), Admitted::Done(e.slug.clone()), "{u}");
    }
    assert_eq!((w.fetch.fetch_calls.get(), w.scorer.calls.get()), (f, s));
    // It took a sitemap position, its shard rebuilds with it, and an
    // IndexNow ping is queued.
    assert!(
        w.h.queue
            .borrow()
            .iter()
            .any(|m| matches!(&m.job, Job::Indexnow { public_slug, .. } if *public_slug == e.slug))
    );
    assert_eq!(block_on(sitemap::rebuild(&w.h, &w.origins, true)).unwrap(), 1);
    let xml = String::from_utf8(w.h.doc(&sitemap::shard_key(0)).unwrap()).unwrap();
    assert!(xml.contains(&format!("https://site.example/d/{}", e.slug)));
    assert_eq!(w.rows("claims"), 0);
}

#[test]
fn a_second_browser_joins_the_claim_and_a_dead_driver_is_taken_over() {
    let w = World::new();
    w.fetch.page(URL, &article(14));
    let Admitted::Drive(first) = w.admit(URL).unwrap() else { panic!() };
    // While the first submitter drives, the next one joins its page.
    assert_eq!(w.admit(URL).unwrap(), Admitted::Joined(first.held.slug.clone()));
    assert_eq!(w.rows("detections"), 1);
    // The pending page polls its state.
    let st = block_on(reads::state(&w.h, &first.held.slug)).unwrap();
    assert_eq!((st.state, st.lease_ended), (DetState::Pending, false));
    // The driver dies; past its lease the next submitter (or "Run it")
    // takes the same page over under a new token.
    w.h.advance(120_001);
    assert!(block_on(reads::state(&w.h, &first.held.slug)).unwrap().lease_ended);
    let Admitted::Drive(second) = w.admit(URL).unwrap() else { panic!() };
    assert_eq!(second.held.slug, first.held.slug);
    assert_ne!(second.held.token, first.held.token);
    // The old driver wakes up: its writes are fenced off.
    let e = block_on(drive(&w.h, &w.host, &w.fetch, &w.scorer, &w.ctx(), &first));
    assert_eq!(e.slug, first.held.slug);
    assert_eq!(w.page(&first.held.slug).unwrap().state, DetState::Pending);
    assert_eq!(w.h.docs.borrow().keys().filter(|k| k.starts_with("r/")).count(), 0, "its document was removed");
    // The new driver finishes the page.
    let e = block_on(drive(&w.h, &w.host, &w.fetch, &w.scorer, &w.ctx(), &second));
    assert_eq!(e.failed, None);
    assert_eq!(w.page(&e.slug).unwrap().state, DetState::Live);
    assert_eq!(w.rows("detections"), 1);
}

#[test]
fn refusals_before_a_slug_store_nothing() {
    let w = World::new();
    w.host.risky.borrow_mut().insert("bad.example.com".into());
    for (u, code) in [
        ("https://example.com/reset/a8F3kQ9zX2mB7nW4pL6vR1tY", RefusalCode::CapabilityUrl),
        ("http://127.0.0.1/a", RefusalCode::BadUrl),
        ("https://bad.example.com/a", RefusalCode::SecurityRisk),
        ("https://www.reddit.com/r/x/comments/abc/title/", RefusalCode::PasteOnly),
        ("https://x.com/someone", RefusalCode::XNotAPost),
        ("https://fxtwitter.com/a/status/1790000000000000000", RefusalCode::XNotAPost),
        ("https://slop.ninja/detect/", RefusalCode::BadUrl),
    ] {
        assert_eq!(w.admit(u), Err(code), "{u}");
    }
    assert_eq!(w.rows("detections") + w.rows("sources") + w.rows("claims"), 0);
    assert_eq!(w.fetch.fetch_calls.get(), 0);
    // Our own page's link answers its slug.
    assert_eq!(w.admit("https://slop.ninja/d/K7M2XQ9").unwrap(), Admitted::Done("k7m2xq9".into()));
    // The switches.
    w.h.vars.borrow_mut().insert("DETECT_URLS".into(), "off".into());
    assert_eq!(w.admit(URL), Err(RefusalCode::UrlsOff));
    w.h.vars.borrow_mut().insert("DETECT_TOOL".into(), "off".into());
    assert_eq!(w.admit(URL), Err(RefusalCode::ToolOff));
}

#[test]
fn failures_end_the_page_with_the_domain_only() {
    let w = World::new();
    w.host.disallowed.borrow_mut().insert(URL.into());
    let e = w.check(URL).unwrap();
    assert_eq!(e.failed, Some(RefusalCode::Robots));
    let p = w.page(&e.slug).unwrap();
    assert_eq!((p.state, p.failure.map(|f| f.code)), (DetState::Failed, Some(RefusalCode::Robots)));
    let src = p.source.unwrap();
    assert_eq!((src.host.as_str(), src.url, src.title), ("example.com", None, None));
    assert_eq!(p.robots, Robots::Noindex);
    // The failure holds for its link for the robots cache period, with no
    // fetch; afterwards the link is checked again.
    assert_eq!(w.admit(URL).unwrap(), Admitted::Done(e.slug.clone()));
    assert_eq!(w.fetch.fetch_calls.get(), 0);
    w.h.advance(24 * 3_600_000 + 1);
    w.host.disallowed.borrow_mut().clear();
    w.fetch.page(URL, &article(14));
    let again = w.check(URL).unwrap();
    assert_ne!(again.slug, e.slug);
    assert_eq!(again.failed, None);
    // Other failures: not found, a login wall, too short, not English.
    for (u, setup, code) in [
        ("https://example.com/missing", None, RefusalCode::FetchFailed),
        ("https://example.com/wall", Some(failed_page(Outcome::LoginWall)), RefusalCode::LoginWall),
        (
            "https://example.com/short",
            Some(ok_page("https://example.com/short", "Too few words here.")),
            RefusalCode::TooShort,
        ),
        (
            "https://example.com/fr",
            Some(ok_page("https://example.com/fr", &format!("[fr] {}", article(14)))),
            RefusalCode::NotEnglish,
        ),
    ] {
        if let Some(a) = setup {
            w.fetch.pages.borrow_mut().insert(u.into(), a);
        }
        let e = w.check(u).unwrap();
        assert_eq!(e.failed, Some(code), "{u}");
        assert_eq!(w.page(&e.slug).unwrap().state, DetState::Failed, "{u}");
    }
    // No failed check kept any text or path.
    let dump = w.h.dump();
    assert!(!dump.contains("/missing") && !dump.contains("/wall") && !dump.contains("Too few words"));
    assert_eq!(w.rows("claims"), 0);
}

#[test]
fn a_failed_call_is_tried_once_more() {
    let w = World::new();
    w.fetch.page(URL, &article(14));
    w.fetch.fail_calls.set(1);
    assert_eq!(w.check(URL).unwrap().failed, None);
    assert_eq!(w.fetch.fetch_calls.get(), 2);
    assert_eq!(w.host.released.get(), 1);
}

#[test]
fn a_canonical_duplicate_becomes_an_alias_and_equal_text_reuses_the_run() {
    let w = World::new();
    let a = w.check_with_final("https://example.com/a?id=1", "https://example.com/story");
    let b = w.check_with_final("https://example.com/b", "https://example.com/story");
    assert_ne!(a, b);
    let pb = w.page(&b).unwrap();
    assert_eq!(pb.canonical.as_deref(), Some(a.as_str()));
    assert_eq!(w.scorer.calls.get(), 1, "the duplicate was not scored");
    assert_eq!(w.rows("runs"), 1);
    // Another page with the same text is a new source sharing the run.
    w.fetch.page("https://example.com/copy", &article(14));
    let c = w.check("https://example.com/copy").unwrap();
    assert_eq!(c.failed, None);
    assert_eq!(w.scorer.calls.get(), 1);
    assert_eq!(w.rows("runs"), 1);
    assert_eq!(w.page(&c.slug).unwrap().state, DetState::Live);
}

impl World {
    fn check_with_final(&self, url: &str, fin: &str) -> String {
        let mut a = ok_page(fin, &article(14));
        a.final_url = Some(fin.into());
        self.fetch.pages.borrow_mut().insert(url.into(), a);
        let e = self.check(url).unwrap();
        assert_eq!(e.failed, None, "{url}");
        e.slug
    }
}

#[test]
fn unranked_and_short_pages_are_not_indexed() {
    let w = World::new();
    w.fetch.page("https://example.org/a", &article(14));
    let e = w.check("https://example.org/a").unwrap();
    let p = w.page(&e.slug).unwrap();
    assert_eq!((p.state, p.robots), (DetState::Live, Robots::Noindex));
    assert!(p.text.unwrap().body.is_none(), "an unranked source shows no excerpt");
    w.fetch.page("https://example.com/short", &format!("{HUMAN} {AI1}"));
    let e = w.check("https://example.com/short").unwrap();
    assert_eq!(w.page(&e.slug).unwrap().robots, Robots::Noindex);
    assert_eq!(block_on(sitemap::rebuild(&w.h, &w.origins, true)).unwrap(), 1);
    let xml = String::from_utf8(w.h.doc(&sitemap::shard_key(0)).unwrap()).unwrap();
    assert!(!xml.contains("<url>"), "{xml}");
}

// ---------------------------------------------------------------------- X

fn x_text() -> String {
    format!("{AI1} {HUMAN}")
}

#[test]
fn an_x_post_is_checked_once_and_keeps_no_text() {
    let w = World::new();
    let id = post_id(w.h.now.get(), 2 * 3_600_000, 1);
    w.fetch.posts.borrow_mut().insert(id, XPostAnswer::Text { text: x_text() });
    let link = format!("https://twitter.com/someone/status/{id}?s=20");
    let e = w.check(&link).unwrap();
    assert_eq!(e.failed, None);
    let p = w.page(&e.slug).unwrap();
    assert_eq!((p.state, p.platform, p.robots), (DetState::Live, Platform::X, Robots::Noindex));
    assert_eq!(p.x.unwrap().post_url.id(), id);
    assert!(p.text.is_none() && p.run.as_ref().unwrap().spans.is_empty());
    // Nothing of the post's text is stored, in D1 or R2.
    let dump = w.h.dump();
    assert!(!dump.contains("fast-paced") && !dump.contains("tomatoes"));
    for (k, (b, _)) in w.h.docs.borrow().iter() {
        let s = String::from_utf8_lossy(b);
        assert!(!s.contains("fast-paced") && !s.contains("tomatoes"), "{k}");
    }
    // No sitemap position and no IndexNow ping.
    assert_eq!(w.h.count("SELECT count(*) FROM detections WHERE sitemap_seq IS NOT NULL"), 0);
    assert!(!w.h.queue.borrow().iter().any(|m| matches!(m.job, Job::Indexnow { .. })));
    // A second check, by any form of the link, makes no request.
    for l in [format!("x.com/i/status/{id}"), format!("https://mobile.x.com/a/status/{id}/photo/1")] {
        assert_eq!(w.admit(&l).unwrap(), Admitted::Done(e.slug.clone()), "{l}");
    }
    assert_eq!(w.fetch.post_calls.get(), 1);
    // No DetectHost call for an X post.
    assert_eq!((w.host.screen_calls.get(), w.host.slot_calls.get()), (0, 0));
}

#[test]
fn a_young_post_makes_no_request_and_draws_no_slug() {
    let w = World::new();
    let now = w.h.now.get();
    let id = post_id(now, 30 * 60_000, 2);
    let r = block_on(admit(&w.h, &w.host, &w.ctx(), &format!("https://x.com/a/status/{id}"))).unwrap_err();
    assert_eq!(r.code, RefusalCode::XPostTooNew);
    assert_eq!(r.after_ms, Some(now - 30 * 60_000 + 3_660_000));
    assert_eq!(w.rows("detections") + w.rows("sources") + w.rows("claims"), 0);
    assert_eq!(w.fetch.post_calls.get(), 0);
    // DETECT_X off, or purged, answers x_off.
    for st in ["off", "purged"] {
        w.h.vars.borrow_mut().insert("DETECT_X".into(), st.into());
        let old = post_id(now, 3 * 3_600_000, 3);
        assert_eq!(w.admit(&format!("https://x.com/a/status/{old}")), Err(RefusalCode::XOff));
    }
}

#[test]
fn refusals_after_the_claim_keep_nothing_about_the_post() {
    let w = World::new();
    let now = w.h.now.get();
    for (n, ans, code) in [
        (10, XPostAnswer::Long, RefusalCode::XPostLong),
        (11, XPostAnswer::Unavailable, RefusalCode::XPostUnavailable),
        (12, XPostAnswer::Busy, RefusalCode::XBusy),
        (13, XPostAnswer::Unexpected, RefusalCode::XUnavailable),
        (14, XPostAnswer::Text { text: "Too short to score.".into() }, RefusalCode::TooShort),
        (15, XPostAnswer::Text { text: format!("[fr] {}", x_text()) }, RefusalCode::NotEnglish),
    ] {
        let id = post_id(now, 2 * 3_600_000, n);
        w.fetch.posts.borrow_mut().insert(id, ans);
        let e = w.check(&format!("https://x.com/a/status/{id}")).unwrap();
        assert_eq!(e.failed, Some(code), "{code}");
        let (state, src): (String, Option<i64>) =
            w.h.db
                .query_row("SELECT state, source_id FROM detections WHERE public_slug = ?1", [&e.slug], |r| {
                    Ok((r.get(0)?, r.get(1)?))
                })
                .unwrap();
        assert_eq!((state.as_str(), src), ("failed", None), "{code}");
    }
    assert_eq!(w.rows("sources") + w.rows("runs") + w.rows("claims"), 0);
    assert!(!w.h.dump().contains("/status/"));
}

#[test]
fn an_existing_post_the_embed_no_longer_shows_is_left_for_the_sweep() {
    let w = World::new();
    let id = post_id(w.h.now.get(), 2 * 3_600_000, 4);
    w.fetch.posts.borrow_mut().insert(id, XPostAnswer::Text { text: x_text() });
    let first = w.check(&format!("https://x.com/a/status/{id}")).unwrap();
    // A newer package: the old page no longer reuses, so the post is asked
    // again, and the embed no longer shows it.
    let mut w2 = w;
    w2.serving.load = "load-next".into();
    w2.fetch.posts.borrow_mut().insert(id, XPostAnswer::Unavailable);
    let e = w2.check(&format!("https://x.com/a/status/{id}")).unwrap();
    assert_eq!(e.failed, Some(RefusalCode::XPostUnavailable));
    let (fetched, url): (i64, Option<String>) =
        w2.h.db.query_row("SELECT fetched_at, canon_url FROM sources", [], |r| Ok((r.get(0)?, r.get(1)?))).unwrap();
    assert_eq!(fetched, 0);
    assert!(url.is_some(), "nothing is deleted here");
    assert_eq!(w2.page(&first.slug).unwrap().state, DetState::Live);
}
