//! The public URL check end to end over the store's own migration (foreign
//! keys on), with counting stubs for the fetch Worker, `DetectHost` and the
//! scorer.

mod common;
mod stubs;

use common::{Harness, block_on};
use slop_ninja_detect_fetch_wire::{Outcome, XPostAnswer};
use slop_ninja_detect_page::codes::{DetState, Platform, Robots, SourceVia};
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
    assert!(w.h.doubles_of("job", "claim_expired").is_empty());
    let Admitted::Drive(second) = w.admit(URL).unwrap() else { panic!() };
    assert_eq!(second.held.slug, first.held.slug);
    assert_ne!(second.held.token, first.held.token);
    // A claim taken over past its lease is counted (`job {claim_expired}`).
    assert_eq!(w.h.doubles_of("job", "claim_expired"), vec![1.0]);
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

/// A pending page names the submitted link's domain and nothing else of
/// it: a web page's and a Bluesky post's from the domain-only source the
/// claim writes in its own batch (only the winner writes it), an X post's
/// `x.com`. The page that goes live shows its own source.
#[test]
fn a_pending_page_names_the_submitted_domain() {
    let w = World::new();
    w.fetch.page(URL, &article(14));
    let bsky = "https://bsky.app/profile/did:plc:z72i7hdynmk6r22z27h6tvur/post/3l6oveex3ii2l";
    let id = post_id(w.h.now.get(), 2 * 3_600_000, 1);
    w.fetch.posts.borrow_mut().insert(id, XPostAnswer::Text { text: x_text() });
    let xlink = format!("https://x.com/someone/status/{id}");
    let mut drives = Vec::new();
    for (link, host, via, platform) in [
        (URL, "example.com", SourceVia::Web, Platform::Web),
        (bsky, "bsky.app", SourceVia::Bluesky, Platform::Bluesky),
        (xlink.as_str(), "x.com", SourceVia::X, Platform::X),
    ] {
        let Admitted::Drive(d) = w.admit(link).unwrap() else { panic!("{link}") };
        let p = w.page(&d.held.slug).unwrap();
        assert_eq!((p.state, p.platform), (DetState::Pending, platform), "{link}");
        let src = p.source.as_ref().unwrap();
        assert_eq!((src.host.as_str(), src.via), (host, via), "{link}");
        assert!(src.url.is_none() && src.title.is_none() && src.page_hash.is_none(), "{link}: the domain only");
        // A second submitter joins the same pending page; no second row.
        assert_eq!(w.admit(link).unwrap(), Admitted::Joined(d.held.slug.clone()), "{link}");
        drives.push(d);
    }
    // One domain-only row for each of the two sites, none for the post;
    // nothing of either link's path.
    assert_eq!(w.rows("sources"), 2);
    assert_eq!(w.h.count("SELECT count(*) FROM detections WHERE source_id IS NULL"), 1, "the X page has none");
    let dump = w.h.dump();
    assert!(!dump.contains("/garden") && !dump.contains("3l6oveex3ii2l") && !dump.contains("someone"));
    // The web check goes live with its own source, not the domain-only row.
    let e = block_on(drive(&w.h, &w.host, &w.fetch, &w.scorer, &w.ctx(), &drives[0]));
    assert_eq!(e.failed, None);
    let p = w.page(&e.slug).unwrap();
    assert_eq!(p.state, DetState::Live);
    let src = p.source.unwrap();
    assert!(src.host == "example.com" && src.url.is_some(), "the page's own source");
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

// ------------------------------------------------- removals and opt-outs

impl World {
    /// A legal state on a public page, as a removal or a withheld excerpt
    /// leaves it: the state and its removal record.
    fn legal(&self, slug: &str, state: &str) {
        legal_on(&self.h.db, slug, state);
    }

    fn state_of(&self, slug: &str) -> (String, Option<String>, Option<i64>, Option<Vec<u8>>) {
        self.h
            .db
            .query_row(
                "SELECT state, refusal, run_id, superseded_by FROM detections WHERE public_slug = ?1",
                [slug],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .unwrap()
    }

    fn run_docs(&self) -> usize {
        self.h.docs.borrow().keys().filter(|k| k.starts_with("r/")).count()
    }
}

fn legal_on(db: &rusqlite::Connection, slug: &str, state: &str) {
    let scope = if state == "excerpt_withheld" { "excerpt" } else { "page" };
    db.execute("UPDATE detections SET state = ?2, state_v = state_v + 1 WHERE public_slug = ?1", [slug, state])
        .unwrap();
    db.execute(
        "INSERT INTO removals (at, public_slug, scope, basis, jurisdiction) VALUES (1, ?1, ?2, 'copyright', 'us')",
        [slug, scope],
    )
    .unwrap();
}

#[test]
fn sending_the_link_of_a_removed_or_withheld_page_again_changes_nothing() {
    for (url, state) in [("https://example.com/a", "removed"), ("https://example.com/b", "excerpt_withheld")] {
        let mut w = World::new();
        w.fetch.page(url, &article(14));
        let e = w.check(url).unwrap();
        assert_eq!(e.failed, None);
        w.legal(&e.slug, state);
        let before = (w.fetch.fetch_calls.get(), w.scorer.calls.get(), w.rows("detections"), w.rows("claims"));
        // The same link in another form, and under a newer package, answers
        // the page under its legal state: no claim, no fetch, no scan.
        for load in ["load-d24", "load-next"] {
            w.serving.load = load.into();
            for u in [url.to_owned(), format!("{url}?utm_source=x#frag")] {
                assert_eq!(w.admit(&u).unwrap(), Admitted::Done(e.slug.clone()), "{state} {load} {u}");
                assert_eq!(w.check(&u).unwrap(), Ended { slug: e.slug.clone(), failed: None });
            }
        }
        assert_eq!(
            (w.fetch.fetch_calls.get(), w.scorer.calls.get(), w.rows("detections"), w.rows("claims")),
            before,
            "{state}"
        );
        assert_eq!(w.h.count("SELECT count(*) FROM detections WHERE state = 'live'"), 0, "{state}");
        let (st, _, _, sup) = w.state_of(&e.slug);
        assert_eq!((st.as_str(), sup), (state, None), "never superseded");
        // The page shows its legal state; a withheld page shows no excerpt.
        w.serving.load = "load-d24".into();
        let p = w.page(&e.slug).unwrap();
        assert_eq!(p.state.as_str(), state);
        if state == "excerpt_withheld" {
            assert!(p.text.unwrap().body.is_none());
        }
    }
}

/// A removed page restored after its run document went comes back failed
/// (`report_gone`), not live and not refetched behind the reader's back;
/// `failed` is no legal state, so the reader's "check the link again" makes
/// a fresh check, scored again (the dead run is not reused).
#[test]
fn a_page_restored_without_its_report_lets_its_link_be_checked_again() {
    let w = World::new();
    let url = "https://example.com/a";
    w.fetch.page(url, &article(14));
    let e = w.check(url).unwrap();
    assert_eq!(e.failed, None);
    let (_, _, run, _) = w.state_of(&e.slug);
    let run = run.unwrap();
    // The removal, and its run document gone with it.
    w.legal(&e.slug, "removed");
    block_on(slop_ninja_detect_worker::db::Io::doc_delete(&w.h, &[format!("r/{run}")])).unwrap();
    let journal = slop_ninja_detect_worker::db::Stmt::new("SELECT 1", Vec::new());
    let r = block_on(slop_ninja_detect_worker::cases::restore_slug(&w.h, &w.origins, &e.slug, journal)).unwrap();
    assert_eq!((r.pages, r.report_gone), (1, 1));
    let p = w.page(&e.slug).unwrap();
    assert_eq!((p.state, p.failure.map(|f| f.code)), (DetState::Failed, Some(RefusalCode::ReportGone)));
    assert_eq!(p.source.unwrap().host, "example.com");
    assert!(!w.h.queue.borrow().iter().any(|m| matches!(m.job, Job::RerunOp { .. })), "no silent fetch");
    // Checking the link again fetches and scores it afresh.
    let (f, sc) = (w.fetch.fetch_calls.get(), w.scorer.calls.get());
    let again = w.check(url).unwrap();
    assert_eq!(again.failed, None);
    assert_ne!(again.slug, e.slug);
    assert_eq!((w.fetch.fetch_calls.get(), w.scorer.calls.get()), (f + 1, sc + 1));
    let p = w.page(&again.slug).unwrap();
    assert_eq!(p.state, DetState::Live);
    assert!(p.text.unwrap().body.is_some());
}

/// A case of the removal form on `slug`, sent.
fn send_case(w: &World, slug: &str, track: slop_ninja_detect_page::codes::CaseKind, fields: &[(&str, &str)]) -> String {
    use slop_ninja_detect_page::api::RemovalRequest;
    use slop_ninja_detect_page::codes::RemovalStep;
    use slop_ninja_detect_worker::cases::{RemovalAnswer, removal};
    let req = RemovalRequest {
        step: RemovalStep::Send,
        track,
        target: slug.into(),
        fields: fields.iter().map(|(k, v)| ((*k).to_owned(), (*v).to_owned())).collect(),
    };
    match block_on(removal(&w.h, Keys { store: &w.store, peppers: &w.peppers }, &w.origins, None, &req)).unwrap() {
        RemovalAnswer::Received(r) => r.case_id,
        other => panic!("{other:?}"),
    }
}

fn case_row(w: &World, case_id: &str) -> slop_ninja_detect_worker::cases::CaseRow {
    block_on(slop_ninja_detect_worker::cases::read_case(&w.h, case_id)).unwrap()
}

/// Two cases on one page, end to end. A copyright case's withhold is
/// accepted (its text rewritten away), then an illegal-content case removes
/// the page and its run document. Restoring the copyright case leaves the
/// page removed, so sending its link again answers the removed page with no
/// claim, fetch or scan, under any package; restoring the removal, the last
/// case holding it, returns it failed, and the link is then checked afresh.
#[test]
fn a_restore_never_frees_a_link_another_case_still_holds() {
    use slop_ninja_detect_page::codes::CaseKind;
    use slop_ninja_detect_worker::cases::{self, Basis};
    use slop_ninja_detect_worker::db::Stmt;
    let mut w = World::new();
    let url = "https://example.com/a";
    w.fetch.page(url, &article(14));
    let e = w.check(url).unwrap();
    assert_eq!(e.failed, None);
    let copyright = [
        ("work", "https://writer.example/essay"),
        ("scope", "excerpt"),
        ("name", "Claimant Fixture Name"),
        ("email", "claimant@writer.invalid"),
        ("address", "1 Fixture Lane"),
        ("good_faith", "yes"),
        ("accurate", "yes"),
        ("signature", "Claimant Fixture Name"),
    ];
    let illegal = [("category", "other"), ("explanation", "it is illegal"), ("good_faith", "yes")];
    let basis = Basis { basis: "other".into(), jurisdiction: "US".into() };
    let journal = || Stmt::new("SELECT 1", Vec::new());
    let b = send_case(&w, &e.slug, CaseKind::Copyright, &copyright);
    assert_eq!(w.state_of(&e.slug).0, "excerpt_withheld");
    block_on(cases::withhold_excerpt(&w.h, &w.origins, &case_row(&w, &b), &basis, journal())).unwrap();
    w.h.advance(1000);
    let a = send_case(&w, &e.slug, CaseKind::Illegal, &illegal);
    block_on(cases::remove_case(&w.h, &w.store, &w.origins, &case_row(&w, &a), &basis, journal())).unwrap();
    assert_eq!((w.state_of(&e.slug).0.as_str(), w.run_docs()), ("removed", 0));
    let r = block_on(cases::restore_case(&w.h, &w.store, &w.origins, &case_row(&w, &b), journal())).unwrap();
    assert_eq!((r.pages, r.on_hold, r.report_gone), (0, 1, 0));
    assert_eq!(w.state_of(&e.slug).0, "removed");
    let before = (w.fetch.fetch_calls.get(), w.scorer.calls.get(), w.rows("detections"), w.rows("claims"));
    let load = w.serving.load.clone();
    for l in [load.as_str(), "load-next"] {
        w.serving.load = l.into();
        assert_eq!(w.admit(url).unwrap(), Admitted::Done(e.slug.clone()), "{l}");
        assert_eq!(w.check(url).unwrap(), Ended { slug: e.slug.clone(), failed: None }, "{l}");
    }
    w.serving.load = load;
    assert_eq!((w.fetch.fetch_calls.get(), w.scorer.calls.get(), w.rows("detections"), w.rows("claims")), before);
    assert_eq!(w.page(&e.slug).unwrap().state, DetState::Removed);
    // The last case holding it, restored: failed, and the link is free.
    let r = block_on(cases::restore_case(&w.h, &w.store, &w.origins, &case_row(&w, &a), journal())).unwrap();
    assert_eq!((r.pages, r.on_hold, r.report_gone), (1, 0, 1));
    let p = w.page(&e.slug).unwrap();
    assert_eq!((p.state, p.failure.map(|f| f.code)), (DetState::Failed, Some(RefusalCode::ReportGone)));
    let again = w.check(url).unwrap();
    assert_eq!(again.failed, None);
    assert_ne!(again.slug, e.slug);
    assert_eq!((w.fetch.fetch_calls.get(), w.scorer.calls.get()), (before.0 + 1, before.1 + 1));
    assert_eq!(w.page(&again.slug).unwrap().state, DetState::Live);
}

/// A removal made outside any case, end to end. A copyright notice
/// withholds the page at intake, then the operator removes it by slug (its
/// run document goes); restoring the notice leaves it removed. A second
/// notice, accepted by a `remove` that finds the page already removed, is
/// restored too and leaves it removed. Sending the link again answers the
/// removed page with no claim, fetch or scan, under any package; once
/// `restore {slug}` returns it failed, the link is checked afresh.
#[test]
fn a_restore_by_case_never_frees_a_link_the_operator_removed() {
    use slop_ninja_detect_page::codes::CaseKind;
    use slop_ninja_detect_worker::cases::{self, Basis};
    use slop_ninja_detect_worker::db::Stmt;
    let mut w = World::new();
    let url = "https://example.com/a";
    w.fetch.page(url, &article(14));
    let e = w.check(url).unwrap();
    assert_eq!(e.failed, None);
    let copyright = |scope: &'static str| {
        [
            ("work", "https://writer.example/essay"),
            ("scope", scope),
            ("name", "Claimant Fixture Name"),
            ("email", "claimant@writer.invalid"),
            ("address", "1 Fixture Lane"),
            ("good_faith", "yes"),
            ("accurate", "yes"),
            ("signature", "Claimant Fixture Name"),
        ]
    };
    let basis = Basis { basis: "other".into(), jurisdiction: "US".into() };
    let journal = || Stmt::new("SELECT 1", Vec::new());
    let b = send_case(&w, &e.slug, CaseKind::Copyright, &copyright("excerpt"));
    assert_eq!(w.state_of(&e.slug).0, "excerpt_withheld");
    let r = block_on(cases::remove_slug(&w.h, &w.origins, &e.slug, &basis, journal())).unwrap();
    assert_eq!(r.pages, 1);
    assert_eq!((w.state_of(&e.slug).0.as_str(), w.run_docs()), ("removed", 0));
    let r = block_on(cases::restore_case(&w.h, &w.store, &w.origins, &case_row(&w, &b), journal())).unwrap();
    assert_eq!((r.pages, r.on_hold, r.report_gone), (0, 1, 0));
    w.h.advance(1000);
    let c = send_case(&w, &e.slug, CaseKind::Copyright, &copyright("page"));
    let r = block_on(cases::remove_case(&w.h, &w.store, &w.origins, &case_row(&w, &c), &basis, journal())).unwrap();
    assert_eq!(r.pages, 0);
    let r = block_on(cases::restore_case(&w.h, &w.store, &w.origins, &case_row(&w, &c), journal())).unwrap();
    assert_eq!((r.pages, r.on_hold, r.report_gone), (0, 1, 0));
    assert_eq!(w.state_of(&e.slug).0, "removed");
    let before = (w.fetch.fetch_calls.get(), w.scorer.calls.get(), w.rows("detections"), w.rows("claims"));
    let load = w.serving.load.clone();
    for l in [load.as_str(), "load-next"] {
        w.serving.load = l.into();
        assert_eq!(w.admit(url).unwrap(), Admitted::Done(e.slug.clone()), "{l}");
        assert_eq!(w.check(url).unwrap(), Ended { slug: e.slug.clone(), failed: None }, "{l}");
    }
    w.serving.load = load;
    assert_eq!((w.fetch.fetch_calls.get(), w.scorer.calls.get(), w.rows("detections"), w.rows("claims")), before);
    assert_eq!(w.page(&e.slug).unwrap().state, DetState::Removed);
    // The operator's own restore: failed, and the link is free.
    let r = block_on(cases::restore_slug(&w.h, &w.origins, &e.slug, journal())).unwrap();
    assert_eq!((r.pages, r.on_hold, r.report_gone), (1, 0, 1));
    let p = w.page(&e.slug).unwrap();
    assert_eq!((p.state, p.failure.map(|f| f.code)), (DetState::Failed, Some(RefusalCode::ReportGone)));
    let again = w.check(url).unwrap();
    assert_eq!(again.failed, None);
    assert_ne!(again.slug, e.slug);
    assert_eq!((w.fetch.fetch_calls.get(), w.scorer.calls.get()), (before.0 + 1, before.1 + 1));
    assert_eq!(w.page(&again.slug).unwrap().state, DetState::Live);
}

#[test]
fn an_opted_out_source_is_never_checked_again() {
    let mut w = World::new();
    w.fetch.page(URL, &article(14));
    let e = w.check(URL).unwrap();
    w.h.exec("UPDATE sources SET state = 'opted_out' WHERE adapter = 'web'");
    w.serving.load = "load-next".into();
    let calls = w.fetch.fetch_calls.get();
    assert_eq!(w.admit(URL), Err(RefusalCode::RerunBlocked));
    assert_eq!(w.fetch.fetch_calls.get(), calls);
    assert_eq!(w.rows("detections"), 1);
    assert_eq!(w.state_of(&e.slug).0, "live");
}

/// One case of a removal landing while a check of the same source runs:
/// the old page's legal state (or the source's opt-out) is applied just
/// before the store's batch, after the fetch and the scan.
fn removal_mid_drive(state: &'static str) {
    let mut w = World::new();
    w.fetch.page(URL, &article(14));
    let first = w.check(URL).unwrap();
    assert_eq!(w.rows("runs"), 1);
    // A newer package: the link is checked again.
    w.serving.load = "load-next".into();
    let Admitted::Drive(d) = w.admit(URL).unwrap() else { panic!("{state}") };
    let old = first.slug.clone();
    w.h.before("INSERT INTO sources (canon_key, canon_url, host, adapter, title", move |db| {
        if state == "opted_out" {
            db.execute("UPDATE sources SET state = 'opted_out' WHERE adapter = 'web'", []).unwrap();
        } else {
            legal_on(db, &old, state);
        }
    });
    let e = block_on(drive(&w.h, &w.host, &w.fetch, &w.scorer, &w.ctx(), &d));
    assert_eq!(w.scorer.calls.get(), 2, "{state}: the check ran to its store");
    assert_eq!(e.failed, Some(RefusalCode::RerunBlocked), "{state}");
    // The new page ended failed with its domain only, and nothing of it
    // stayed: no run row, no run document, no claim.
    let (st, refusal, run, _) = w.state_of(&e.slug);
    assert_eq!((st.as_str(), refusal.as_deref(), run), ("failed", Some("rerun_blocked"), None), "{state}");
    let p = w.page(&e.slug).unwrap();
    assert_eq!((p.state, p.failure.map(|f| f.code)), (DetState::Failed, Some(RefusalCode::RerunBlocked)));
    let src = p.source.unwrap();
    assert_eq!((src.host.as_str(), src.url, src.title), ("example.com", None, None));
    assert_eq!((w.rows("runs"), w.run_docs(), w.rows("claims")), (1, 1, 0), "{state}");
    assert_eq!(w.h.count("SELECT count(*) FROM detections WHERE sitemap_seq IS NOT NULL"), 1);
    assert!(
        !w.h.queue
            .borrow()
            .iter()
            .any(|m| matches!(&m.job, Job::Indexnow { public_slug, .. } if *public_slug == e.slug))
    );
    // The old page keeps its state and is superseded by nothing, and the
    // source stays opted out.
    let (old_state, _, _, sup) = w.state_of(&first.slug);
    let want = if state == "opted_out" { "live" } else { state };
    assert_eq!((old_state.as_str(), sup), (want, None), "{state}");
    let sstate: String =
        w.h.db.query_row("SELECT state FROM sources WHERE canon_url = ?1", [URL], |r| r.get(0)).unwrap();
    assert_eq!(sstate, if state == "opted_out" { "opted_out" } else { "ok" });
    // Sending the link again makes no fetch.
    let calls = w.fetch.fetch_calls.get();
    match w.admit(URL) {
        Ok(Admitted::Done(s)) => assert_eq!(s, first.slug, "{state}"),
        Err(code) => assert_eq!((state, code), ("opted_out", RefusalCode::RerunBlocked)),
        other => panic!("{state}: {other:?}"),
    }
    assert_eq!(w.fetch.fetch_calls.get(), calls);
}

#[test]
fn a_removal_that_lands_during_a_check_wins() {
    for state in ["removed", "excerpt_withheld", "opted_out"] {
        removal_mid_drive(state);
    }
}

#[test]
fn a_removal_seen_after_the_fetch_ends_the_check_before_the_scan() {
    let mut w = World::new();
    w.fetch.page(URL, &article(14));
    let first = w.check(URL).unwrap();
    w.serving.load = "load-next".into();
    let Admitted::Drive(d) = w.admit(URL).unwrap() else { panic!() };
    w.legal(&first.slug, "removed");
    let e = block_on(drive(&w.h, &w.host, &w.fetch, &w.scorer, &w.ctx(), &d));
    assert_eq!(e.failed, Some(RefusalCode::RerunBlocked));
    assert_eq!(w.scorer.calls.get(), 1, "no scan");
    assert_eq!((w.rows("runs"), w.run_docs(), w.rows("claims")), (1, 1, 0));
    assert_eq!(w.state_of(&e.slug).0, "failed");
}

// ------------------------------------------------- declared canonicals

#[test]
fn a_declared_canonical_never_names_another_page() {
    // Two authors on one registrable domain. The page at A declares B, an
    // article by someone else, as its canonical, and is checked first.
    let w = World::new();
    let (a, b) = ("https://blog.example.com/someone/a", "https://blog.example.com/victim/b");
    let mut fa = ok_page(a, &article(14));
    fa.canonical = Some(b.into());
    fa.meta.as_mut().unwrap().title = Some("Not the victim's title".into());
    w.fetch.pages.borrow_mut().insert(a.into(), fa);
    let mut fb = ok_page(b, &vec![HUMAN; 14].join(" "));
    fb.meta.as_mut().unwrap().title = Some("The victim's own article".into());
    w.fetch.pages.borrow_mut().insert(b.into(), fb);
    let ea = w.check(a).unwrap();
    assert_eq!(ea.failed, None);
    let pa = w.page(&ea.slug).unwrap();
    assert_eq!(pa.source.as_ref().unwrap().url.as_deref(), Some(a), "A is its own source");
    // Nothing names B yet: a check of B fetches and scores B.
    let (f, s) = (w.fetch.fetch_calls.get(), w.scorer.calls.get());
    let eb = w.check(b).unwrap();
    assert_eq!(eb.failed, None);
    assert_ne!(eb.slug, ea.slug);
    assert_eq!((w.fetch.fetch_calls.get(), w.scorer.calls.get()), (f + 1, s + 1));
    let pb = w.page(&eb.slug).unwrap();
    let src = pb.source.as_ref().unwrap();
    assert_eq!((src.url.as_deref(), src.title.as_deref()), (Some(b), Some("The victim's own article")));
    let run = pb.run.as_ref().unwrap();
    assert_eq!(run.band, Some(slop_ninja_detect_page::codes::Band::Weak), "B's own result");
    assert!(!pb.text.as_ref().unwrap().body.as_deref().unwrap_or("").contains(AI1));
    assert_eq!(pb.canonical.as_deref(), Some(eb.slug.as_str()));
    // A's link still answers A's page; B's answers B's.
    assert_eq!(w.admit(a).unwrap(), Admitted::Done(ea.slug.clone()));
    assert_eq!(w.admit(b).unwrap(), Admitted::Done(eb.slug.clone()));
    // No source is keyed, named or aliased by a declared canonical.
    assert_eq!(w.h.count("SELECT count(*) FROM source_aliases"), 0);
}

#[test]
fn a_declared_canonical_joins_a_live_page_of_it() {
    // B is checked first; A then declares B and becomes an alias of B's page
    // with B's real result, with no scan.
    let w = World::new();
    let (a, b) = ("https://example.com/print/7", "https://example.com/story/7");
    w.fetch.page(b, &article(14));
    let mut fa = ok_page(a, &vec![HUMAN; 14].join(" "));
    fa.canonical = Some(b.into());
    w.fetch.pages.borrow_mut().insert(a.into(), fa);
    let eb = w.check(b).unwrap();
    let s = w.scorer.calls.get();
    let ea = w.check(a).unwrap();
    assert_eq!(ea.failed, None);
    assert_eq!(w.scorer.calls.get(), s, "no scan");
    let pa = w.page(&ea.slug).unwrap();
    assert_eq!(pa.canonical.as_deref(), Some(eb.slug.as_str()));
    assert_eq!(pa.source.as_ref().unwrap().url.as_deref(), Some(b));
    // A's link now answers B's page.
    assert_eq!(w.admit(a).unwrap(), Admitted::Done(eb.slug.clone()));
    // A declared canonical on another registrable domain is ignored.
    let c = "https://example.com/c";
    let mut fc = ok_page(c, &vec![HUMAN; 13].join(" "));
    fc.canonical = Some("https://example.org/story/7".into());
    w.fetch.pages.borrow_mut().insert(c.into(), fc);
    let ec = w.check(c).unwrap();
    assert_eq!(w.page(&ec.slug).unwrap().source.unwrap().url.as_deref(), Some(c));
}

#[test]
fn a_legal_state_blocks_although_an_alias_page_of_it_is_live() {
    // A short link to the page, checked a minute after it, made an alias
    // page of it (live, superseded_by the page, created after it). The
    // alias page never lifts the block a legal state on the page sets.
    for state in ["excerpt_withheld", "removed"] {
        let w = World::new();
        w.fetch.page(URL, &article(14));
        let first = w.check(URL).unwrap();
        assert_eq!(first.failed, None);
        w.h.advance(60_000);
        let short = "https://example.com/s/abc";
        let alias = w.check_with_final(short, URL);
        assert_ne!(alias, first.slug);
        let (st, _, _, sup) = w.state_of(&alias);
        assert_eq!((st.as_str(), sup.is_some()), ("live", true), "{state}");
        w.legal(&first.slug, state);
        w.h.advance(60_000);
        let before = (w.fetch.fetch_calls.get(), w.scorer.calls.get(), w.rows("detections"), w.rows("claims"));
        for u in [URL, short] {
            assert_eq!(w.admit(u).unwrap(), Admitted::Done(first.slug.clone()), "{state} {u}");
            assert_eq!(w.check(u).unwrap(), Ended { slug: first.slug.clone(), failed: None }, "{state} {u}");
        }
        assert_eq!(
            (w.fetch.fetch_calls.get(), w.scorer.calls.get(), w.rows("detections"), w.rows("claims")),
            before,
            "{state}: no fetch, scan, page or claim"
        );
        assert_eq!(
            w.h.count("SELECT count(*) FROM detections WHERE state = 'live'"),
            1,
            "{state}: the alias page only"
        );
        assert_eq!(w.state_of(&first.slug).0, state);
        // A new link that redirects to the page: the block shows only after
        // the fetch, while the alias page is live. It makes no new alias
        // page; its pending page ends failed.
        let other = "https://example.com/s/def";
        let mut a = ok_page(URL, &article(14));
        a.final_url = Some(URL.into());
        w.fetch.pages.borrow_mut().insert(other.into(), a);
        let e = w.check(other).unwrap();
        assert_eq!(e.failed, Some(RefusalCode::RerunBlocked), "{state}");
        assert_eq!(w.state_of(&e.slug).0, "failed", "{state}");
        assert_eq!(
            w.h.count("SELECT count(*) FROM detections WHERE state = 'live'"),
            1,
            "{state}: still the alias page only"
        );
    }
}

#[test]
fn a_legal_state_blocks_although_a_newer_page_of_its_source_is_live() {
    // The page under an older package, and the newer page that superseded
    // it; then a legal state on the older one.
    let mut w = World::new();
    w.fetch.page(URL, &article(14));
    let old = w.check(URL).unwrap();
    w.h.advance(60_000);
    w.serving.load = "load-next".into();
    let newer = w.check(URL).unwrap();
    assert_ne!(newer.slug, old.slug);
    assert!(w.state_of(&old.slug).3.is_some(), "superseded by the newer page");
    w.legal(&old.slug, "removed");
    w.h.advance(60_000);
    let before = (w.fetch.fetch_calls.get(), w.scorer.calls.get(), w.rows("detections"), w.rows("claims"));
    // Under the newer page's package the link answers that page, which
    // already existed; under another package, the removed one. Neither
    // fetches or makes a page.
    assert_eq!(w.admit(URL).unwrap(), Admitted::Done(newer.slug.clone()));
    w.serving.load = "load-later".into();
    assert_eq!(w.admit(URL).unwrap(), Admitted::Done(old.slug.clone()));
    assert_eq!(w.check(URL).unwrap(), Ended { slug: old.slug.clone(), failed: None });
    assert_eq!((w.fetch.fetch_calls.get(), w.scorer.calls.get(), w.rows("detections"), w.rows("claims")), before);
    // Under the newer page's package, a new link that redirects to the page:
    // the block shows only after the fetch, while the newer page is live. It
    // joins no alias to it; its own page ends failed.
    w.serving.load = "load-next".into();
    let other = "https://example.com/s/ghi";
    let mut a = ok_page(URL, &article(14));
    a.final_url = Some(URL.into());
    w.fetch.pages.borrow_mut().insert(other.into(), a);
    let e = w.check(other).unwrap();
    assert_eq!(e.failed, Some(RefusalCode::RerunBlocked));
    assert_eq!(w.state_of(&e.slug).3, None, "not an alias of the newer page");
    assert_eq!(w.h.count("SELECT count(*) FROM detections WHERE state = 'live'"), 1, "the newer page only");
}

// ------------------------------------------------- redirects to other sites

fn host_of(u: &str) -> String {
    url::Url::parse(u).unwrap().host_str().unwrap().to_owned()
}

#[test]
fn a_redirect_to_another_site_is_judged_by_that_site() {
    // example.com is ranked. Its links redirect to an unranked site and to
    // an adult one: each page names the site the text came from, and shows
    // no title or excerpt and stays out of the index, as a page of that site.
    let w = World::new();
    w.host.adult.borrow_mut().insert("example.net".into());
    for (link, fin) in [
        ("https://example.com/go/1", "https://www.example.org/story"),
        ("https://example.com/go/2", "https://example.net/story"),
    ] {
        let slug = w.check_with_final(link, fin);
        let p = w.page(&slug).unwrap();
        let src = p.source.as_ref().unwrap();
        assert_eq!((src.host.as_str(), src.title.as_deref()), (host_of(fin).as_str(), None), "{fin}");
        assert!(p.text.as_ref().unwrap().body.is_none(), "{fin}: no excerpt");
        assert_eq!(p.robots, Robots::Noindex, "{fin}");
        let card = slop_ninja_detect_page::card::Card::from_page(&p, false).unwrap();
        assert_eq!(card.source_label, host_of(fin), "{fin}");
    }
    assert_eq!(block_on(sitemap::rebuild(&w.h, &w.origins, true)).unwrap(), 1);
    let xml = String::from_utf8(w.h.doc(&sitemap::shard_key(0)).unwrap()).unwrap();
    assert!(!xml.contains("<url>"), "{xml}");
    // The other way round: an unranked site's link to a ranked site's page
    // is that site's page.
    let slug = w.check_with_final("https://example.org/go/3", "https://example.com/story/3");
    let p = w.page(&slug).unwrap();
    let src = p.source.as_ref().unwrap();
    assert_eq!((src.host.as_str(), src.title.as_deref()), ("example.com", Some("Garden notes")));
    assert_eq!(p.robots, Robots::Index);
}

#[test]
fn a_redirect_into_an_opted_out_host_keeps_nothing() {
    let w = World::new();
    w.h.exec("INSERT INTO optouts (host, at, journal_seq) VALUES ('www.example.org', 1, 1)");
    let (link, fin) = ("https://example.com/go/4", "https://www.example.org/story/4");
    let mut a = ok_page(fin, &article(14));
    a.final_url = Some(fin.into());
    w.fetch.pages.borrow_mut().insert(link.into(), a);
    let e = w.check(link).unwrap();
    assert_eq!(e.failed, Some(RefusalCode::PrivateSource));
    assert_eq!(w.scorer.calls.get(), 0, "no scan");
    let p = w.page(&e.slug).unwrap();
    assert_eq!((p.state, p.failure.map(|f| f.code)), (DetState::Failed, Some(RefusalCode::PrivateSource)));
    let src = p.source.unwrap();
    assert_eq!((src.host.as_str(), src.url, src.title), ("example.com", None, None));
    // Nothing of the page or its site is kept.
    assert_eq!((w.rows("runs"), w.run_docs(), w.rows("claims")), (0, 0, 0));
    assert_eq!(w.h.count("SELECT count(*) FROM sources WHERE host = 'www.example.org'"), 0);
    let dump = w.h.dump();
    assert!(!dump.contains("/story/4") && !dump.contains("tomatoes"));
    // The failure answers for the link, with no fetch, for the robots cache
    // period.
    let calls = w.fetch.fetch_calls.get();
    assert_eq!(w.admit(link).unwrap(), Admitted::Done(e.slug.clone()));
    assert_eq!(w.fetch.fetch_calls.get(), calls);
}

#[test]
fn a_declared_canonical_must_be_on_the_site_the_text_came_from() {
    // B on example.com has a live page. A link on example.com redirects to
    // a page on example.org that declares B its canonical: B is not on the
    // text's site, so the page is its own and B's page is untouched.
    let w = World::new();
    let b = "https://example.com/story/9";
    w.fetch.page(b, &article(14));
    let eb = w.check(b).unwrap();
    let (link, fin) = ("https://example.com/go/9", "https://example.org/copy/9");
    let mut a = ok_page(fin, &vec![HUMAN; 14].join(" "));
    a.final_url = Some(fin.into());
    a.canonical = Some(b.into());
    w.fetch.pages.borrow_mut().insert(link.into(), a);
    let s = w.scorer.calls.get();
    let e = w.check(link).unwrap();
    assert_eq!(e.failed, None);
    assert_ne!(e.slug, eb.slug);
    assert_eq!(w.scorer.calls.get(), s + 1, "scored as its own page");
    let p = w.page(&e.slug).unwrap();
    assert_eq!(p.source.as_ref().unwrap().host, "example.org");
    assert_eq!(p.canonical.as_deref(), Some(e.slug.as_str()));
    assert_eq!(w.admit(b).unwrap(), Admitted::Done(eb.slug.clone()));
    assert_eq!(w.admit(link).unwrap(), Admitted::Done(e.slug.clone()));
}
