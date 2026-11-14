//! Pages about posts on X after they are made: the daily existence check,
//! deletion (R2 first, then D1, in foreign-key order), removals, restore
//! and the purge, on the store's own migration with foreign keys on.

mod common;
mod stubs;

use common::{Harness, block_on};
use slop_ninja_detect_fetch_wire::{XControl, XExists, XFailure, XPostAnswer};
use slop_ninja_detect_page::codes::{DetState, XDeleteCause};
use slop_ninja_detect_page::origins::Origins;
use slop_ninja_detect_page::queue::Job;
use slop_ninja_detect_page::refusal::RefusalCode;
use slop_ninja_detect_url::Policy;
use slop_ninja_detect_worker::doc::Serving;
use slop_ninja_detect_worker::keys::{Pepper, Peppers, StoreKeys};
use slop_ninja_detect_worker::share::Keys;
use slop_ninja_detect_worker::url_flow::{Admitted, UrlCtx, admit, drive};
use slop_ninja_detect_worker::x_flow::{
    self, CaseRef, Removal, X_CHECK_EVERY_MS, X_LATE_MS, X_REQUEUE_MS, XConfig, XReason, x_canon_key,
};
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

fn remove(w: &World, id: u64, case: CaseRef, auto: bool) -> x_flow::Removed {
    let r = Removal { canon_key: x_canon_key(id), reason: XReason::Author, case, auto_journal: auto };
    block_on(x_flow::remove_post(&w.h, &w.origins, &r, Vec::new())).unwrap()
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
    let r = remove(&w, id, CaseRef::New("td-x0000001".into()), false);
    assert_eq!((r.case_id.as_str(), r.removed), ("td-x0000001", 2));
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
    assert!(block_on(x_flow::restore_x(&w.h, "td-x0000001", Vec::new())).unwrap());
    assert_eq!(w.h.count("SELECT count(*) FROM sources"), 0);
    assert_eq!(w.page(&s1).unwrap().state, DetState::Removed);
    let st: String = w.h.db.query_row("SELECT state FROM cases", [], |r| r.get(0)).unwrap();
    assert_eq!(st, "restored");
    assert!(!block_on(x_flow::restore_x(&w.h, "td-x0000001", Vec::new())).unwrap(), "already restored");
    let Admitted::Drive(d) = block_on(admit(&w.h, &w.host, &w.ctx(), &format!("https://x.com/i/status/{id}"))).unwrap()
    else {
        panic!()
    };
    let s3 = block_on(drive(&w.h, &w.host, &w.fetch, &w.scorer, &w.ctx(), &d)).slug;
    assert!(s3 != s1 && s3 != s2);
    // x_remove of a post with no page writes its tombstone.
    let never = post_id(w.h.now.get(), 3 * 3_600_000, 9);
    let r = remove(&w, never, CaseRef::New("td-x0000002".into()), false);
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
    let r = remove(&w, id, CaseRef::Existing("td-form0001".into()), true);
    assert_eq!(r.removed, 1);
    let (actor, op): (String, String) =
        w.h.db.query_row("SELECT actor, op FROM op_journal", [], |r| Ok((r.get(0)?, r.get(1)?))).unwrap();
    assert_eq!(actor, x_flow::AUTO_TOMBSTONE_ACTOR);
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
    remove(&w, id, CaseRef::New("td-x0000003".into()), true);
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
