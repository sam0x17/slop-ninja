//! Pages about posts on X after they are made: the daily existence check,
//! deletion (R2 first, then D1, in foreign-key order), removals (through
//! `cases::x_removal`, the one path the form, `remove` and `x_remove`
//! take), restore and the purge, on the store's own migration with foreign
//! keys on.

mod common;
mod stubs;

use common::{AT_DOC_DELETE, Harness, block_on, interleave};
use slop_ninja_detect_fetch_wire::{XControl, XExists, XFailure, XPostAnswer};
use slop_ninja_detect_page::codes::{DetState, XDeleteCause};
use slop_ninja_detect_page::origins::Origins;
use slop_ninja_detect_page::queue::Job;
use slop_ninja_detect_page::refusal::RefusalCode;
use slop_ninja_detect_url::Policy;
use slop_ninja_detect_worker::cases::{self, XCase, XReason};
use slop_ninja_detect_worker::db::Stmt;
use slop_ninja_detect_worker::doc::Serving;
use slop_ninja_detect_worker::keys::{Pepper, Peppers, StoreKeys};
use slop_ninja_detect_worker::share::Keys;
use slop_ninja_detect_worker::url_flow::{Admitted, UrlCtx, admit, drive};
use slop_ninja_detect_worker::x_flow::{self, X_CHECK_EVERY_MS, X_LATE_MS, X_REQUEUE_MS, XConfig, x_canon_key};
use slop_ninja_detect_worker::{queue, reads};
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
    cfg: XConfig,
}

impl World {
    fn new(batch: u64) -> World {
        let h = Harness::new();
        for (k, v) in [("DETECT_TOOL", "on"), ("DETECT_URLS", "on"), ("DETECT_X", "on"), ("DETECT_PAGES", "on")] {
            h.vars.borrow_mut().insert(k.into(), v.into());
        }
        World {
            h,
            fetch: FetchStub::default(),
            host: HostStub::default(),
            scorer: ScorerStub::default(),
            store: StoreKeys::derive(&[7u8; 32]),
            peppers: Peppers::new(vec![Pepper::new(1, [3u8; 32])]).unwrap(),
            origins: Origins::from_vars("https://site.example", "https://s.site.example").unwrap(),
            serving: serving(),
            policy: Policy::from_origins(Some("https://site.example"), None),
            cfg: XConfig { call_ms: 10_000, batch },
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

    /// A live X page for post number `n` (created two hours ago).
    fn post(&self, n: u64) -> (u64, String) {
        let id = post_id(self.h.now.get(), 2 * 3_600_000, n);
        self.fetch.posts.borrow_mut().insert(id, XPostAnswer::Text { text: format!("{AI1} {HUMAN}") });
        let a = block_on(admit(&self.h, &self.host, &self.ctx(), &format!("https://x.com/i/status/{id}"))).unwrap();
        let Admitted::Drive(d) = a else { panic!("{a:?}") };
        let e = block_on(drive(&self.h, &self.host, &self.fetch, &self.scorer, &self.ctx(), &d));
        assert_eq!(e.failed, None);
        (id, e.slug)
    }

    fn sweep(&self) -> x_flow::SweepReport {
        block_on(x_flow::sweep(&self.h, &self.fetch, &self.origins, &self.cfg)).unwrap()
    }

    fn page(&self, slug: &str) -> Result<slop_ninja_detect_page::page::Page, RefusalCode> {
        block_on(reads::page(&self.h, Keys { store: &self.store, peppers: &self.peppers }, &self.serving, slug))
    }

    fn fetched(&self, id: u64) -> Option<i64> {
        self.h
            .db
            .query_row(
                "SELECT fetched_at FROM sources WHERE canon_url = ?1",
                [format!("https://x.com/i/status/{id}")],
                |r| r.get(0),
            )
            .ok()
    }

    /// The source id of post `id`.
    fn sid(&self, id: u64) -> u64 {
        let s: i64 = self
            .h
            .db
            .query_row(
                "SELECT source_id FROM sources WHERE canon_url = ?1",
                [format!("https://x.com/i/status/{id}")],
                |r| r.get(0),
            )
            .unwrap();
        s as u64
    }

    fn claimed(&self, id: u64) -> Option<i64> {
        self.h
            .db
            .query_row(
                "SELECT check_queued_at FROM sources WHERE canon_url = ?1",
                [format!("https://x.com/i/status/{id}")],
                |r| r.get(0),
            )
            .unwrap()
    }

    fn points(&self, name: &str) -> Vec<(String, f64)> {
        self.h.doubles.borrow().iter().filter(|(p, _, _)| p == name).map(|(_, b, d)| (b[0].clone(), *d)).collect()
    }

    /// No run row that no page names, no run document without its row, and
    /// no run row without its document.
    fn nothing_orphaned(&self, tag: &str) {
        let unnamed = self.h.count(
            "SELECT count(*) FROM runs r WHERE NOT EXISTS (SELECT 1 FROM detections d WHERE d.run_id = r.run_id)",
        );
        assert_eq!(unnamed, 0, "{tag}: a run no page names");
        let mut rows: Vec<String> = self
            .h
            .db
            .prepare("SELECT doc_key FROM runs")
            .unwrap()
            .query_map([], |r| r.get(0))
            .unwrap()
            .map(Result::unwrap)
            .collect();
        rows.sort();
        let docs: Vec<String> = self.h.docs.borrow().keys().filter(|k| k.starts_with("r/")).cloned().collect();
        assert_eq!(docs, rows, "{tag}: run documents and run rows");
    }

    fn state_of(&self, slug: &str) -> (String, Option<String>) {
        self.h
            .db
            .query_row("SELECT state, refusal FROM detections WHERE public_slug = ?1", [slug], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .unwrap()
    }

    fn reset_counts(&self) {
        self.h.doubles.borrow_mut().clear();
        self.h.metrics.borrow_mut().clear();
        self.h.queue.borrow_mut().clear();
        self.fetch.asked.borrow_mut().clear();
    }
}

const DAY: u64 = 24 * 3_600_000;

#[test]
fn present_posts_move_their_check_and_gone_posts_are_deleted() {
    let w = World::new(50);
    let (a, sa) = w.post(1);
    let (b, sb) = w.post(2);
    // Not due yet: an empty sweep makes no call and writes no xcheck point.
    w.reset_counts();
    let r = w.sweep();
    assert_eq!((r.calls, r.batch.len(), r.next), (0, 0, false));
    assert!(w.points("xcheck").is_empty());
    assert_eq!(w.points("xsweep"), vec![("empty".to_owned(), 0.0)]);
    // A day later both are due; b is gone at X.
    w.h.advance(X_CHECK_EVERY_MS + 1);
    w.fetch.exists.borrow_mut().insert(b, XExists::Gone);
    w.reset_counts();
    let r = w.sweep();
    assert_eq!(r.calls, 1);
    assert_eq!(w.fetched(a), Some(w.h.now.get() as i64));
    assert_eq!(w.claimed(a), None);
    assert_eq!(r.gone.len(), 1);
    // The gone post's page answers 410 x_gone; its run, document and source
    // went, its slug stays as a tombstone.
    let p = w.page(&sb).unwrap();
    assert_eq!((p.state, p.ended.map(|e| e.code)), (DetState::Deleted, Some(RefusalCode::XGone)));
    assert_eq!(p.http_status(), 410);
    assert_eq!(w.fetched(b), None);
    assert_eq!(w.h.count("SELECT count(*) FROM runs"), 1);
    assert_eq!(w.h.docs.borrow().keys().filter(|k| k.starts_with("r/")).count(), 1);
    assert_eq!(w.page(&sa).unwrap().state, DetState::Live);
    // One xcheck point per outcome with its count, one xsweep point.
    let mut pts = w.points("xcheck");
    pts.sort_by(|x, y| x.0.cmp(&y.0));
    assert_eq!(pts, vec![("gone".to_owned(), 1.0), ("present".to_owned(), 1.0)]);
    assert_eq!(w.points("xsweep").len(), 1);
    // The slug of an X page never reaches a point.
    for (_, blobs) in w.h.metrics.borrow().iter() {
        assert!(!blobs.iter().any(|b| *b == sa || *b == sb));
    }
    // The control posts are asked by detect-fetch, never named here.
    assert_eq!(*w.fetch.asked.borrow(), vec![a, b]);
}

#[test]
fn failed_controls_record_nothing() {
    let w = World::new(50);
    let (a, _) = w.post(1);
    w.h.advance(X_CHECK_EVERY_MS + 1);
    let before = w.fetched(a);
    for c in [XControl::Live, XControl::Gone] {
        w.fetch.controls_fail.set(Some(c));
        w.reset_counts();
        let r = w.sweep();
        assert!(r.stopped && !r.next);
        assert_eq!(w.fetched(a), before, "no fetched_at moves");
        assert_eq!(w.claimed(a), None, "the claims are cleared");
        assert_eq!(w.points("xcheck"), vec![("control".to_owned(), 1.0)]);
        assert!(w.h.queue.borrow().is_empty());
    }
    assert_eq!(w.h.count("SELECT count(*) FROM sources WHERE canon_url IS NOT NULL"), 1);
}

#[test]
fn a_failure_mid_batch_stops_the_chain_and_one_bad_post_goes_last() {
    let w = World::new(8);
    let ids: Vec<u64> = (1..=8).map(|n| w.post(n).0).collect();
    w.h.advance(X_CHECK_EVERY_MS + 1);
    // The second post answers 5xx every time.
    let bad = ids[1];
    w.fetch.exists.borrow_mut().insert(bad, XExists::Failed { why: XFailure::Server });
    w.reset_counts();
    let r = w.sweep();
    assert!(r.stopped && !r.next, "a failure stops the chain");
    // The first round (6 in flight) finished; the rest were not asked.
    assert_eq!(w.fetch.asked.borrow().len(), 6);
    assert!(w.fetch.max_in_flight.get() <= 6);
    let now = w.h.now.get() as i64;
    for id in &ids[..6] {
        if *id == bad {
            assert!(w.claimed(*id).is_some(), "the failed post keeps its claim");
        } else {
            assert_eq!(w.fetched(*id), Some(now));
        }
    }
    for id in &ids[6..] {
        assert_eq!((w.claimed(*id), w.fetched(*id) == Some(now)), (None, false), "not asked: claim cleared, kept due");
    }
    // The next cron checks the rest first; the bad post waits out its claim.
    w.h.advance(10 * 60_000);
    w.reset_counts();
    let r = w.sweep();
    assert_eq!(r.batch, ids[6..].iter().map(|i| w.sid(*i)).collect::<Vec<_>>());
    // Once X_REQUEUE_S has passed it is asked again, last in its batch.
    w.h.advance(X_REQUEUE_MS);
    for id in &ids {
        if *id != bad {
            w.h.db
                .execute(
                    "UPDATE sources SET fetched_at = 0 WHERE canon_url = ?1",
                    [format!("https://x.com/i/status/{id}")],
                )
                .unwrap();
        }
    }
    w.reset_counts();
    let _ = w.sweep();
    assert_eq!(w.fetch.asked.borrow().last(), Some(&bad), "{:?}", w.fetch.asked.borrow());
    // After 24 hours unchecked, status names its slug.
    w.h.advance(X_LATE_MS);
    let st = block_on(x_flow::status(&w.h, "", 10)).unwrap();
    assert!(st.oldest_check_s >= X_LATE_MS / 1000);
    assert!(!st.late_pages.is_empty());
}

#[test]
fn the_bad_post_is_last_even_with_the_lowest_id_and_oldest_check() {
    let w = World::new(4);
    let ids: Vec<u64> = (1..=4).map(|n| w.post(n).0).collect();
    let bad = ids[0];
    w.h.db
        .execute("UPDATE sources SET fetched_at = 1 WHERE canon_url = ?1", [format!("https://x.com/i/status/{bad}")])
        .unwrap();
    w.h.advance(X_CHECK_EVERY_MS + 1);
    w.fetch.exists.borrow_mut().insert(bad, XExists::Unexpected);
    w.reset_counts();
    let _ = w.sweep();
    assert_eq!(w.fetch.asked.borrow().first(), Some(&bad), "first cron: oldest first");
    w.h.advance(X_REQUEUE_MS + 1);
    for id in &ids[1..] {
        w.h.db
            .execute("UPDATE sources SET fetched_at = 2 WHERE canon_url = ?1", [format!("https://x.com/i/status/{id}")])
            .unwrap();
    }
    w.reset_counts();
    let _ = w.sweep();
    assert_eq!(w.fetch.asked.borrow().last(), Some(&bad), "{:?}", w.fetch.asked.borrow());
    assert_eq!(w.points("xcheck").iter().find(|(o, _)| o == "unexpected").map(|x| x.1), Some(1.0));
}

#[test]
fn a_full_batch_sends_the_next_message_and_chains_claim_disjoint_batches() {
    let w = World::new(3);
    let ids: Vec<u64> = (1..=7).map(|n| w.post(n).0).collect();
    w.h.advance(X_CHECK_EVERY_MS + 1);
    // Another chain claims the second source between this SELECT and UPDATE.
    let other = ids[1];
    let now = w.h.now.get();
    w.h.before("UPDATE sources SET check_queued_at", move |db| {
        db.execute(
            "UPDATE sources SET check_queued_at = ?1 WHERE canon_url = ?2",
            rusqlite::params![now as i64, format!("https://x.com/i/status/{other}")],
        )
        .unwrap();
    });
    w.reset_counts();
    let r = w.sweep();
    assert_eq!(r.batch, vec![w.sid(ids[0]), w.sid(ids[2])], "the rest keep their order");
    assert!(r.full && r.next);
    assert!(matches!(w.h.queue.borrow().last().map(|m| &m.job), Some(Job::XSweep {})));
    // Three batches' worth take several messages, each one call.
    let mut calls = 0;
    for _ in 0..5 {
        let r = w.sweep();
        calls += r.calls;
        if !r.next {
            break;
        }
    }
    assert!(calls >= 2);
    let late = w.h.count("SELECT count(*) FROM sources WHERE canon_url IS NOT NULL AND fetched_at < (SELECT max(fetched_at) FROM sources)");
    assert!(late <= 1, "every source but the other chain's is checked");
}

#[test]
fn ae_writes_do_not_grow_with_the_batch() {
    let w = World::new(60);
    let ids: Vec<u64> = (1..=60).map(|n| w.post(n).0).collect();
    w.h.advance(X_CHECK_EVERY_MS + 1);
    // Every outcome at once: gone, unexpected (stops the rest: not_checked).
    for id in &ids[..5] {
        w.fetch.exists.borrow_mut().insert(*id, XExists::Gone);
    }
    w.fetch.exists.borrow_mut().insert(ids[10], XExists::Unexpected);
    w.reset_counts();
    let _ = w.sweep();
    let n = w.h.metrics.borrow().len();
    assert!(n <= 6, "{n} points: {:?}", w.h.metrics.borrow());
    let total: f64 = w.points("xcheck").iter().map(|x| x.1).sum();
    assert_eq!(total, 60.0);
}

#[test]
fn an_r2_failure_leaves_d1_and_x_delete_finishes() {
    let w = World::new(10);
    let (a, sa) = w.post(1);
    w.h.advance(X_CHECK_EVERY_MS + 1);
    w.fetch.exists.borrow_mut().insert(a, XExists::Gone);
    w.h.fail_doc_delete.set(true);
    w.reset_counts();
    let _ = w.sweep();
    assert!(w.fetched(a).is_some(), "D1 unchanged");
    assert_eq!(w.page(&sa).unwrap().state, DetState::Live);
    let msgs: Vec<_> = w.h.queue.borrow().iter().map(|m| m.job.clone()).collect();
    let src: i64 = w.h.db.query_row("SELECT source_id FROM sources", [], |r| r.get(0)).unwrap();
    assert!(msgs.contains(&Job::XDelete { source_id: src as u64, cause: XDeleteCause::XGone }));
    w.h.fail_doc_delete.set(false);
    block_on(x_flow::x_delete(&w.h, &w.origins, src as u64, XDeleteCause::XGone)).unwrap();
    assert_eq!(w.fetched(a), None);
    assert_eq!(w.h.docs.borrow().keys().filter(|k| k.starts_with("r/")).count(), 0);
    assert_eq!(w.page(&sa).unwrap().state, DetState::Deleted);
    // Repeating it is harmless.
    block_on(x_flow::x_delete(&w.h, &w.origins, src as u64, XDeleteCause::XGone)).unwrap();
}

#[test]
fn a_page_whose_document_went_answers_503_until_its_rows_go() {
    let w = World::new(10);
    let (_, sa) = w.post(1);
    w.h.docs.borrow_mut().retain(|k, _| !k.starts_with("r/"));
    assert_eq!(w.page(&sa), Err(RefusalCode::ReportUnavailable));
}

/// The X removal of post `id` as the routes run it: `x_remove` (no journal
/// row here: the op route writes its own) or, with `auto`, the form and
/// `remove`, which journal `auto:x_tombstone`.
async fn removal(w: &World, id: u64, case: XCase<'_>, auto: bool) -> Result<cases::XRemoved, RefusalCode> {
    let key = x_canon_key(id);
    let journal = if auto { vec![cases::auto_x_journal(&key, XReason::Author, w.h.now.get())] } else { Vec::new() };
    cases::x_removal(&w.h, &w.origins, &key, XReason::Author, case, journal).await
}

fn remove(w: &World, id: u64, case: XCase<'_>, auto: bool) -> cases::XRemoved {
    block_on(removal(w, id, case, auto)).unwrap()
}

/// `restore {case_id}` as the op route runs it; the pages whose tombstone
/// it lifted (0 or 1).
fn restore(w: &World, case_id: &str) -> u64 {
    let case = block_on(cases::read_case(&w.h, case_id)).unwrap();
    let journal = Stmt::new(
        "INSERT INTO op_journal (at, actor, op, signature, nonce_h) VALUES (1, 'operator:00', '{\"op\":\"restore\"}', NULL, NULL)",
        vec![],
    );
    block_on(cases::restore_case(&w.h, &w.store, &w.origins, &case, journal)).unwrap().pages
}

#[test]
fn removals_leave_the_tombstone_and_restore_lifts_it() {
    let mut w = World::new(10);
    let (id, s1) = w.post(1);
    // A second page of the same post under a newer package.
    w.serving.load = "load-next".into();
    w.fetch.posts.borrow_mut().insert(id, XPostAnswer::Text { text: format!("{AI1} {HUMAN}") });
    let Admitted::Drive(d) = block_on(admit(&w.h, &w.host, &w.ctx(), &format!("https://x.com/i/status/{id}"))).unwrap()
    else {
        panic!()
    };
    let s2 = block_on(drive(&w.h, &w.host, &w.fetch, &w.scorer, &w.ctx(), &d)).slug;
    let r = remove(&w, id, XCase::New, false);
    assert_eq!(r.removed, 2);
    for s in [&s1, &s2] {
        let p = w.page(s).unwrap();
        assert_eq!((p.state, p.http_status()), (DetState::Removed, 451));
        assert!(p.removal.is_some());
    }
    // Only the key is kept.
    let (url, st): (Option<String>, String) =
        w.h.db.query_row("SELECT canon_url, state FROM sources", [], |r| Ok((r.get(0)?, r.get(1)?))).unwrap();
    assert_eq!((url, st.as_str()), (None, "opted_out"));
    assert_eq!(w.h.count("SELECT count(*) FROM runs"), 0);
    assert_eq!(w.h.docs.borrow().keys().filter(|k| k.starts_with("r/")).count(), 0);
    let kind: String = w.h.db.query_row("SELECT kind FROM cases", [], |r| r.get(0)).unwrap();
    assert_eq!(kind, "x_request");
    assert_eq!(w.h.count("SELECT count(*) FROM case_targets"), 2);
    // A later check answers x_removed with no request.
    let calls = w.fetch.post_calls.get();
    let a = block_on(admit(&w.h, &w.host, &w.ctx(), &format!("https://x.com/a/status/{id}"))).unwrap_err();
    assert_eq!(a.code, RefusalCode::XRemoved);
    assert_eq!(w.fetch.post_calls.get(), calls);
    // No point, journal row or queue message holds the post's id or link.
    assert!(!w.h.dump().contains(&id.to_string()) && !w.h.dump().contains("/status/"));
    // restore lifts the opt-out: the detections stay removed, the tombstone
    // goes, and a new check scores under a new slug.
    assert_eq!(restore(&w, &r.case_id), 1);
    assert_eq!(w.h.count("SELECT count(*) FROM sources"), 0);
    assert_eq!(w.page(&s1).unwrap().state, DetState::Removed);
    let st: String = w.h.db.query_row("SELECT state FROM cases", [], |r| r.get(0)).unwrap();
    assert_eq!(st, "restored");
    assert_eq!(restore(&w, &r.case_id), 0, "already restored");
    let Admitted::Drive(d) = block_on(admit(&w.h, &w.host, &w.ctx(), &format!("https://x.com/i/status/{id}"))).unwrap()
    else {
        panic!()
    };
    let s3 = block_on(drive(&w.h, &w.host, &w.fetch, &w.scorer, &w.ctx(), &d)).slug;
    assert!(s3 != s1 && s3 != s2);
    // x_remove of a post with no page writes its tombstone.
    let never = post_id(w.h.now.get(), 3 * 3_600_000, 9);
    let r = remove(&w, never, XCase::New, false);
    assert_eq!(r.removed, 0);
    assert_eq!(
        block_on(admit(&w.h, &w.host, &w.ctx(), &format!("https://x.com/i/status/{never}"))).unwrap_err().code,
        RefusalCode::XRemoved
    );
}

#[test]
fn the_form_removal_uses_its_case_and_journals_the_hash() {
    let w = World::new(10);
    let (id, _) = w.post(1);
    w.h.exec(
        "INSERT INTO cases (case_id, status_h, kind, state, received_at, updated_at, notice_key) \
         VALUES ('td-form0001', x'01', 'author', 'received', 1, 1, 'c/td-form0001')",
    );
    let r = remove(&w, id, XCase::Existing { case_id: "td-form0001", insert: Vec::new() }, true);
    assert_eq!((r.case_id.as_str(), r.removed), ("td-form0001", 1));
    let (actor, op): (String, String) =
        w.h.db.query_row("SELECT actor, op FROM op_journal", [], |r| Ok((r.get(0)?, r.get(1)?))).unwrap();
    assert_eq!(actor, "auto:x_tombstone");
    assert!(op.contains(&hex::encode(x_canon_key(id))) && !op.contains(&id.to_string()));
    let src: Option<i64> = w.h.db.query_row("SELECT source_id FROM cases", [], |r| r.get(0)).unwrap();
    assert!(src.is_some());
}

#[test]
fn the_purge_deletes_every_x_row_and_strips_the_journal() {
    let w = World::new(2);
    let mut slugs = Vec::new();
    for n in 1..=5 {
        slugs.push(w.post(n).1);
    }
    // One post removed (its tombstone named by a case and a journal row).
    let id = post_id(w.h.now.get(), 2 * 3_600_000, 1);
    remove(&w, id, XCase::New, true);
    let c = block_on(x_flow::purge_counts(&w.h)).unwrap();
    assert!(!c.is_zero());
    assert_eq!((c.runs, c.cases, c.journal), (4, 1, 1));
    // The op's batch: its journal row, DETECT_X purged, the first page.
    let (stmts, first) = x_flow::purge_start(w.h.now.get(), 1);
    w.h.exec("INSERT INTO op_journal (at, actor, op, signature, nonce_h) VALUES (1, 'operator:00', '{\"op\":\"x_purge\"}', NULL, NULL)");
    block_on(slop_ninja_detect_worker::db::Sql::batch(&w.h, stmts)).unwrap();
    assert!(matches!(first.job, Job::XPurgePage { journal_seq: 1 }));
    // slugs[0] was removed and stays removed (451); the rest answer 410.
    for s in &slugs[1..] {
        let p = w.page(s).unwrap();
        assert_eq!((p.state, p.ended.map(|e| e.code)), (DetState::Deleted, Some(RefusalCode::XPurged)), "{s}");
    }
    let mut pages = 0;
    loop {
        let p = block_on(x_flow::purge_page(&w.h, &w.origins, &w.cfg, 1)).unwrap();
        pages += 1;
        assert!(w.h.metrics.borrow().iter().filter(|(n, _)| n == "xcheck").count() == 0);
        if !p.next {
            break;
        }
    }
    assert!(pages >= 3, "a purge over several pages");
    let c = block_on(x_flow::purge_counts(&w.h)).unwrap();
    assert!(c.is_zero(), "{c:?}");
    assert_eq!(w.h.count("SELECT count(*) FROM sources WHERE adapter = 'x'"), 0);
    assert!(!w.h.dump().contains(&hex::encode(x_canon_key(id))));
    assert_eq!(w.h.docs.borrow().keys().filter(|k| k.starts_with("r/")).count(), 0);
    // Through the queue consumer as well.
    let ping = PingStub::default();
    let d = queue::UrlDeps {
        fetch: &w.fetch,
        ping: &ping,
        origins: &w.origins,
        serving: &w.serving,
        keys: Keys { store: &w.store, peppers: &w.peppers },
        indexnow_key: None,
    };
    assert_eq!(block_on(queue::consume_url(&w.h, &d, &first)), Some(queue::Ack::Done));
}

#[test]
fn replay_makes_every_x_source_due() {
    let w = World::new(10);
    let (a, _) = w.post(1);
    w.h.exec(x_flow::REPLAY_RECHECK);
    assert_eq!(w.fetched(a), Some(0));
    w.reset_counts();
    assert_eq!(w.sweep().batch.len(), 1);
    let _ = DAY;
}

#[test]
fn every_point_written_is_in_the_schema() {
    use slop_ninja_detect_page::ae::detect;
    let w = World::new(10);
    let (a, _) = w.post(1);
    let (b, _) = w.post(2);
    w.h.advance(X_CHECK_EVERY_MS + 1);
    w.fetch.exists.borrow_mut().insert(a, XExists::Gone);
    w.fetch.exists.borrow_mut().insert(b, XExists::Failed { why: XFailure::Timeout });
    let _ = w.sweep();
    w.h.advance(X_CHECK_EVERY_MS + 1);
    w.fetch.controls_fail.set(Some(XControl::Live));
    let _ = w.sweep();
    let _ = w.sweep();
    for (name, blobs) in w.h.metrics.borrow().iter() {
        let set: &[&str] = match name.as_str() {
            "xcheck" => detect::XCHECK_OUTCOMES,
            "xsweep" => detect::XSWEEP_STEPS,
            "xread" => detect::XREAD_OUTCOMES,
            "reuse" => detect::REUSE_KINDS,
            _ => continue,
        };
        assert!(set.contains(&blobs[0].as_str()), "{name} {blobs:?}");
        let p = detect::ALL.iter().find(|p| p.name == name).unwrap();
        assert_eq!(p.blobs.len(), blobs.len(), "{name}");
    }
}

#[test]
fn a_removal_during_a_check_leaves_nothing_of_the_post() {
    // The removal lands after admit read no tombstone and before the store
    // (the pending page has no source yet, so the removal cannot name it).
    for at_store in [false, true] {
        let w = World::new(10);
        let id = post_id(w.h.now.get(), 2 * 3_600_000, 1);
        w.fetch.posts.borrow_mut().insert(id, XPostAnswer::Text { text: format!("{AI1} {HUMAN}") });
        let a = block_on(admit(&w.h, &w.host, &w.ctx(), &format!("https://x.com/i/status/{id}"))).unwrap();
        let Admitted::Drive(d) = a else { panic!("{a:?}") };
        if at_store {
            // Inside the store's own batch, just before its first statement.
            let key = x_canon_key(id).to_vec();
            w.h.before("INSERT INTO sources (canon_key, canon_url, host, adapter, fetched_at", move |db| {
                db.execute(x_flow::UPSERT_TOMBSTONE, [&key]).unwrap();
            });
        } else {
            let r = remove(&w, id, XCase::New, false);
            assert_eq!(r.removed, 0, "the pending page is not the removal's");
        }
        let e = block_on(drive(&w.h, &w.host, &w.fetch, &w.scorer, &w.ctx(), &d));
        assert_eq!(e.failed, Some(RefusalCode::XRemoved), "at_store {at_store}");
        assert_eq!(w.scorer.calls.get(), 1, "the check ran to its store");
        // No live page, no run row, no run document, no claim; the page
        // ended failed with no source.
        let (state, refusal, src, run): (String, Option<String>, Option<i64>, Option<i64>) =
            w.h.db
                .query_row(
                    "SELECT state, refusal, source_id, run_id FROM detections WHERE public_slug = ?1",
                    [&e.slug],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
                )
                .unwrap();
        assert_eq!((state.as_str(), refusal.as_deref(), src, run), ("failed", Some("x_removed"), None, None));
        assert_eq!(w.h.count("SELECT count(*) FROM detections WHERE state = 'live'"), 0);
        assert_eq!(w.h.count("SELECT count(*) FROM runs"), 0);
        assert_eq!(w.h.docs.borrow().keys().filter(|k| k.starts_with("r/")).count(), 0);
        assert_eq!(w.h.count("SELECT count(*) FROM claims"), 0);
        let p = w.page(&e.slug).unwrap();
        assert_eq!((p.state, p.failure.map(|f| f.code)), (DetState::Failed, Some(RefusalCode::XRemoved)));
        // Only the tombstone is left, and a later check answers x_removed
        // with no request.
        let (url, st): (Option<String>, String) =
            w.h.db.query_row("SELECT canon_url, state FROM sources", [], |r| Ok((r.get(0)?, r.get(1)?))).unwrap();
        assert_eq!((url, st.as_str()), (None, "opted_out"));
        let calls = w.fetch.post_calls.get();
        let r = block_on(admit(&w.h, &w.host, &w.ctx(), &format!("https://x.com/a/status/{id}"))).unwrap_err();
        assert_eq!(r.code, RefusalCode::XRemoved);
        assert_eq!(w.fetch.post_calls.get(), calls);
        assert!(!w.h.dump().contains("/status/"));
    }
}

// ------------------------------------------------- removals racing checks

/// The gaps of a removal a check's store can land in, by name and the
/// pause's needle: before its tombstone, before its read of the post's
/// pages and runs, before its R2 delete and before its D1 batch.
const REMOVAL_GAPS: [(&str, &str); 4] = [
    ("tombstone", x_flow::UPSERT_TOMBSTONE),
    ("read", "SELECT DISTINCT run_id FROM detections"),
    ("r2", AT_DOC_DELETE),
    ("batch", "SET state = 'removed', refusal = 'x_removed'"),
];

#[test]
fn a_check_that_races_a_removal_leaves_nothing_of_the_post() {
    // The post's first check, and a check under a newer package of a post
    // that has a page: the check runs to its end in each gap of the removal
    // (a concurrent invocation), and nothing of the post may be left.
    for recheck in [false, true] {
        for (gap, needle) in REMOVAL_GAPS {
            // A first check holds no run when the removal reads: no R2 delete.
            if !recheck && gap == "r2" {
                continue;
            }
            let tag = format!("recheck {recheck}, gap {gap}");
            let mut w = World::new(10);
            let id = post_id(w.h.now.get(), 2 * 3_600_000, 1);
            w.fetch.posts.borrow_mut().insert(id, XPostAnswer::Text { text: format!("{AI1} {HUMAN}") });
            let first = recheck.then(|| w.post(1).1);
            w.serving.load = "load-next".into();
            let Admitted::Drive(d) =
                block_on(admit(&w.h, &w.host, &w.ctx(), &format!("https://x.com/i/status/{id}"))).unwrap()
            else {
                panic!("{tag}")
            };
            w.h.pause_at(needle);
            let (removed, ended) =
                interleave(removal(&w, id, XCase::New, false), drive(&w.h, &w.host, &w.fetch, &w.scorer, &w.ctx(), &d));
            let removed = removed.unwrap();
            assert!(w.h.pauses.borrow().is_empty(), "{tag}: the removal paused there");
            assert_eq!(w.scorer.calls.get(), 1 + u32::from(recheck), "{tag}: the check ran to its store");
            // The check stored before the tombstone, and the removal took its
            // page; or it ended x_removed, keeping nothing.
            let (state, refusal) = w.state_of(&ended.slug);
            if gap == "tombstone" {
                assert_eq!((state.as_str(), ended.failed), ("removed", None), "{tag}");
            } else {
                assert_eq!((state.as_str(), ended.failed), ("failed", Some(RefusalCode::XRemoved)), "{tag}");
            }
            assert_eq!(refusal.as_deref(), Some("x_removed"), "{tag}");
            let took = u64::from(recheck) + u64::from(state == "removed");
            assert_eq!(removed.removed, took, "{tag}");
            if let Some(s) = &first {
                let p = w.page(s).unwrap();
                assert_eq!((p.state, p.http_status()), (DetState::Removed, 451), "{tag}");
            }
            // No live page, run row, run document or claim; only the
            // tombstone, which the case names.
            assert_eq!(w.h.count("SELECT count(*) FROM detections WHERE state = 'live'"), 0, "{tag}");
            assert_eq!(w.h.count("SELECT count(*) FROM runs"), 0, "{tag}");
            assert_eq!(w.h.docs.borrow().keys().filter(|k| k.starts_with("r/")).count(), 0, "{tag}");
            assert_eq!(w.h.count("SELECT count(*) FROM claims"), 0, "{tag}");
            let (url, st): (Option<String>, String) =
                w.h.db.query_row("SELECT canon_url, state FROM sources", [], |r| Ok((r.get(0)?, r.get(1)?))).unwrap();
            assert_eq!((url, st.as_str()), (None, "opted_out"), "{tag}");
            assert_eq!(w.h.count("SELECT count(*) FROM cases WHERE source_id IS NOT NULL"), 1, "{tag}");
            assert!(w.h.queue.borrow().iter().all(|m| !matches!(m.job, Job::XDelete { .. })), "{tag}");
            // A later check answers x_removed with no request; the sweep has
            // nothing to ask; the purge's dry run counts no run.
            let calls = w.fetch.post_calls.get();
            let a = block_on(admit(&w.h, &w.host, &w.ctx(), &format!("https://x.com/a/status/{id}"))).unwrap_err();
            assert_eq!((a.code, w.fetch.post_calls.get()), (RefusalCode::XRemoved, calls), "{tag}");
            w.h.advance(X_CHECK_EVERY_MS + 1);
            let checks = w.fetch.check_calls.get();
            let _ = w.sweep();
            assert_eq!(w.fetch.check_calls.get(), checks, "{tag}");
            let c = block_on(x_flow::purge_counts(&w.h)).unwrap();
            assert_eq!((c.runs, c.run_docs), (0, 0), "{tag}");
            assert!(!w.h.dump().contains("/status/"), "{tag}");
        }
    }
}

#[test]
fn a_run_the_removal_did_not_read_is_deleted_by_its_last_read() {
    // A page and its run reach the post's source inside the removal's own
    // batch, past the store's fence. No flow can do this (the test writes
    // the rows itself, the document first, as a store would); the removal's
    // last read finds them and deletes them the same way.
    let w = World::new(10);
    let (id, s1) = w.post(1);
    let sid = w.sid(id);
    w.h.docs.borrow_mut().insert("r/4242".into(), (b"run".to_vec(), 0));
    w.h.before("SET state = 'removed', refusal = 'x_removed'", move |db| {
        db.execute(
            "INSERT INTO runs (run_id, content_key, package_load, package_pub, package_label, policy_v, canon_v, \
             extract_v, display_v, evidence_id, band, words, total_words, cut, tokens, scored_at, doc_key, doc_bytes) \
             SELECT 4242, NULL, package_load, package_pub, package_label, policy_v, canon_v, extract_v, display_v, \
             evidence_id, band, words, total_words, cut, tokens, scored_at, 'r/4242', doc_bytes FROM runs LIMIT 1",
            [],
        )
        .unwrap();
        db.execute(
            "INSERT INTO detections (id_h, public_slug, kind, platform, state, created_at, updated_at, source_id, \
             run_id, package_load) VALUES (x'42', 'zz42zz4', 'public_url', 'x', 'live', 1, 1, ?1, 4242, 'load-next')",
            [sid as i64],
        )
        .unwrap();
    });
    let r = remove(&w, id, XCase::New, false);
    assert_eq!(r.removed, 2, "both pages");
    assert_eq!(w.page(&s1).unwrap().state, DetState::Removed);
    assert_eq!(w.h.count("SELECT count(*) FROM detections WHERE state = 'live'"), 0);
    assert_eq!(w.h.count("SELECT count(*) FROM detections WHERE run_id IS NOT NULL"), 0);
    assert_eq!(w.h.count("SELECT count(*) FROM runs"), 0);
    assert_eq!(w.h.docs.borrow().keys().filter(|k| k.starts_with("r/")).count(), 0);
    assert!(w.h.queue.borrow().iter().all(|m| !matches!(m.job, Job::XDelete { .. })));
    w.nothing_orphaned("last read");
}

#[test]
fn a_check_that_stores_during_a_gone_deletion_orphans_nothing() {
    // The sweep found the post gone at X; a check of it under a newer
    // package runs to its end in each gap of the deletion.
    for gap in ["SELECT DISTINCT run_id FROM detections", AT_DOC_DELETE, "THEN state ELSE 'deleted' END"] {
        let mut w = World::new(10);
        let (id, s1) = w.post(1);
        w.h.advance(X_CHECK_EVERY_MS + 1);
        w.fetch.exists.borrow_mut().insert(id, XExists::Gone);
        w.serving.load = "load-next".into();
        let Admitted::Drive(d) =
            block_on(admit(&w.h, &w.host, &w.ctx(), &format!("https://x.com/i/status/{id}"))).unwrap()
        else {
            panic!("{gap}")
        };
        w.h.pause_at(gap);
        let (rep, ended) = interleave(
            x_flow::sweep(&w.h, &w.fetch, &w.origins, &w.cfg),
            drive(&w.h, &w.host, &w.fetch, &w.scorer, &w.ctx(), &d),
        );
        assert_eq!(rep.unwrap().gone.len(), 1, "{gap}");
        assert!(w.h.pauses.borrow().is_empty(), "{gap}: the deletion paused there");
        assert_eq!(ended.failed, None, "{gap}: the check stored");
        w.nothing_orphaned(gap);
        let p1 = w.page(&s1).unwrap();
        assert_eq!((p1.state, p1.ended.map(|e| e.code)), (DetState::Deleted, Some(RefusalCode::XGone)), "{gap}");
        if gap == "SELECT DISTINCT run_id FROM detections" {
            // It stored before the read: the deletion took its page too.
            assert_eq!(w.page(&ended.slug).unwrap().state, DetState::Deleted, "{gap}");
        } else {
            // It stored after the read: its page stays whole, with its run,
            // document and source, until the next sweep finds the post gone.
            assert_eq!(w.page(&ended.slug).unwrap().state, DetState::Live, "{gap}");
            assert_eq!(w.h.count("SELECT count(*) FROM sources WHERE canon_url IS NOT NULL"), 1, "{gap}");
            w.h.advance(X_CHECK_EVERY_MS + 1);
            assert_eq!(w.sweep().gone.len(), 1, "{gap}");
            assert_eq!(w.page(&ended.slug).unwrap().state, DetState::Deleted, "{gap}");
        }
        assert_eq!(w.h.count("SELECT count(*) FROM sources") + w.h.count("SELECT count(*) FROM runs"), 0, "{gap}");
        assert_eq!(w.h.docs.borrow().keys().filter(|k| k.starts_with("r/")).count(), 0, "{gap}");
    }
}

#[test]
fn a_gone_deletion_keeps_a_removal_that_landed_while_x_was_asked() {
    // The sweep claimed the post and X answered gone; before the deletion
    // reads, a removal of the post runs to its end.
    let w = World::new(10);
    let (id, s1) = w.post(1);
    w.h.advance(X_CHECK_EVERY_MS + 1);
    w.fetch.exists.borrow_mut().insert(id, XExists::Gone);
    w.h.pause_at("SELECT DISTINCT run_id FROM detections");
    let (rep, removed) =
        interleave(x_flow::sweep(&w.h, &w.fetch, &w.origins, &w.cfg), removal(&w, id, XCase::New, false));
    rep.unwrap();
    let removed = removed.unwrap();
    assert_eq!(removed.removed, 1);
    assert!(w.h.pauses.borrow().is_empty());
    // The tombstone, the case's link to it and the removed page stay.
    let (url, st): (Option<String>, String) =
        w.h.db.query_row("SELECT canon_url, state FROM sources", [], |r| Ok((r.get(0)?, r.get(1)?))).unwrap();
    assert_eq!((url, st.as_str()), (None, "opted_out"));
    assert_eq!(w.h.count("SELECT count(*) FROM cases WHERE source_id IS NOT NULL"), 1);
    let p = w.page(&s1).unwrap();
    assert_eq!((p.state, p.http_status()), (DetState::Removed, 451));
    let calls = w.fetch.post_calls.get();
    let a = block_on(admit(&w.h, &w.host, &w.ctx(), &format!("https://x.com/a/status/{id}"))).unwrap_err();
    assert_eq!((a.code, w.fetch.post_calls.get()), (RefusalCode::XRemoved, calls));
    w.nothing_orphaned("gone after removal");
    assert_eq!(w.h.count("SELECT count(*) FROM runs"), 0);
    // restore still lifts it.
    assert_eq!(restore(&w, &removed.case_id), 1);
    assert_eq!(w.h.count("SELECT count(*) FROM sources"), 0);
}
