//! The detect store's migration (`subnet/detect-worker/migrations/`) applied
//! to an in-memory SQLite with foreign keys on, as D1 runs it: every CHECK
//! and UNIQUE is exercised, the CHECK lists equal this crate's value sets,
//! and the statements the design relies on behave as it says.

#![allow(clippy::too_many_arguments)]

use rusqlite::types::Value as V;
use rusqlite::{Connection, params_from_iter};
use slop_ninja_detect_page::*;

const SCHEMA: &str = include_str!("../../../detect-worker/migrations/0001_detect_store.sql");

fn db() -> Connection {
    let c = Connection::open_in_memory().unwrap();
    c.pragma_update(None, "foreign_keys", true).unwrap();
    c.execute_batch(SCHEMA).unwrap();
    c
}

fn run(c: &Connection, sql: &str, p: &[V]) -> Result<usize, String> {
    c.execute(sql, params_from_iter(p.iter())).map_err(|e| e.to_string())
}

fn refused(c: &Connection, sql: &str, p: &[V], what: &str) {
    let e = run(c, sql, p).expect_err(sql);
    assert!(e.contains(what), "{sql}: expected {what}, got {e}");
}

fn b(x: u8) -> V {
    V::Blob(vec![x; 32])
}
fn t(s: &str) -> V {
    V::Text(s.into())
}
fn i(n: i64) -> V {
    V::Integer(n)
}
const NULL: V = V::Null;

/// The quoted strings of every `<col> IN (…)` list in the schema.
fn in_lists(col: &str) -> Vec<Vec<String>> {
    let needle = format!("CHECK ({col} IN (");
    let mut out = Vec::new();
    let mut rest = SCHEMA;
    while let Some(at) = rest.find(&needle) {
        let after = &rest[at + needle.len()..];
        let end = after.find(')').unwrap();
        out.push(after[..end].split(',').map(|s| s.trim().trim_matches('\'').to_owned()).collect());
        rest = &after[end..];
    }
    assert!(!out.is_empty(), "no IN list for {col}");
    out
}

fn same<T: Copy>(col: &str, all: &[T], s: impl Fn(T) -> &'static str) {
    let want: Vec<String> = all.iter().map(|x| s(*x).to_owned()).collect();
    for list in in_lists(col) {
        assert_eq!(list, want, "{col}");
    }
}

#[test]
fn check_lists_equal_the_value_sets() {
    same("platform", Platform::ALL, Platform::as_str);
    same("owner_via", OwnerVia::ALL, OwnerVia::as_str);
    same("index_force", IndexForce::ALL, IndexForce::as_str);
    same("adapter", Adapter::ALL, Adapter::as_str);
    same("reputation", Reputation::ALL, Reputation::as_str);
    same("review", StrikeReview::ALL, StrikeReview::as_str);
    same("source", ExampleSource::ALL, ExampleSource::as_str);
    // `state` has four lists: sources, detections, jobs and cases, in order.
    let states = in_lists("state");
    let names = |a: &[&'static str]| a.iter().map(|s| (*s).to_owned()).collect::<Vec<_>>();
    assert_eq!(states[0], names(&SourceStatus::ALL.iter().map(|s| s.as_str()).collect::<Vec<_>>()));
    assert_eq!(states[1], names(&DetState::ALL.iter().map(|s| s.as_str()).collect::<Vec<_>>()));
    assert_eq!(states[2], names(&JobState::ALL.iter().map(|s| s.as_str()).collect::<Vec<_>>()));
    assert_eq!(states[3], names(&CaseState::ALL.iter().map(|s| s.as_str()).collect::<Vec<_>>()));
    // `kind` has two: detections, then cases.
    let kinds = in_lists("kind");
    assert_eq!(kinds[0], names(&Kind::STORED.iter().map(|s| s.as_str()).collect::<Vec<_>>()));
    assert_eq!(kinds[1], names(&CaseKind::ALL.iter().map(|s| s.as_str()).collect::<Vec<_>>()));
}

#[test]
fn schema_info_names_the_schema() {
    let c = db();
    let v: String = c.query_row("SELECT v FROM schema_info WHERE k = 'schema'", [], |r| r.get(0)).unwrap();
    assert_eq!(v, "slop_ninja_detect_store_v1");
}

const SRC: &str = "INSERT INTO sources (source_id, canon_key, canon_url, host, adapter, state, reputation, rank_list_v) \
                   VALUES (?, ?, ?, ?, ?, ?, ?, ?)";
const RUN: &str = "INSERT INTO runs (run_id, content_key, package_load, package_pub, package_label, policy_v, canon_v, \
                   display_v, evidence_id, band, words, total_words, cut, tokens, scored_at, doc_key, doc_bytes) \
                   VALUES (?, ?, 'l', 'p', 'span package d24', 'long-v1', 'canon_v1', 'display_v1', 'e', 'strong', \
                   300, 300, 'none', 400, 1, 'r/1', 10)";
const DET: &str = "INSERT INTO detections (id_h, public_slug, kind, platform, state, created_at, updated_at, owner_h, \
                   owner_via, seal_nonce_h, sitemap_seq, index_force) VALUES (?, ?, ?, ?, ?, 1, 1, ?, ?, ?, ?, ?)";

fn det(id: u8, slug: V, kind: &str, state: &str, owner: V, via: V, seal: V, seq: V, force: V) -> Vec<V> {
    vec![b(id), slug, t(kind), t("paste"), t(state), owner, via, seal, seq, force]
}

#[test]
fn sources_and_runs() {
    let c = db();
    let src = |id: i64, key: u8, adapter: &str, state: &str, rep: &str| {
        vec![i(id), b(key), t("https://a.example/"), t("a.example"), t(adapter), t(state), t(rep), t("r1")]
    };
    run(&c, SRC, &src(1, 1, "web", "ok", "ranked")).unwrap();
    refused(&c, SRC, &src(2, 1, "web", "ok", "ranked"), "UNIQUE constraint failed: sources.canon_key");
    refused(&c, SRC, &src(3, 3, "rss", "ok", "ranked"), "CHECK constraint failed");
    refused(&c, SRC, &src(4, 4, "web", "dead", "ranked"), "CHECK constraint failed");
    refused(&c, SRC, &src(5, 5, "web", "ok", "famous"), "CHECK constraint failed");
    run(&c, "INSERT INTO source_aliases VALUES (?, 1)", &[b(9)]).unwrap();
    refused(&c, "INSERT INTO source_aliases VALUES (?, 77)", &[b(8)], "FOREIGN KEY constraint failed");
    run(&c, RUN, &[i(1), b(1)]).unwrap();
    refused(&c, RUN, &[i(2), b(1)], "UNIQUE constraint failed: runs.content_key");
    // Private runs carry no content key; NULLs never collide.
    run(&c, RUN, &[i(3), NULL]).unwrap();
    run(&c, RUN, &[i(4), NULL]).unwrap();
}

#[test]
fn detections_checks_and_uniques() {
    let c = db();
    run(&c, DET, &det(1, t("k7m2xq9"), "public_url", "live", NULL, NULL, NULL, i(1), NULL)).unwrap();
    refused(
        &c,
        DET,
        &det(2, t("k7m2xq9"), "public_url", "live", NULL, NULL, NULL, NULL, NULL),
        "detections.public_slug",
    );
    refused(&c, DET, &det(3, NULL, "secret", "live", NULL, NULL, NULL, NULL, NULL), "CHECK constraint failed");
    refused(
        &c,
        DET,
        &det(4, NULL, "private_text", "archived", NULL, NULL, NULL, NULL, NULL),
        "CHECK constraint failed",
    );
    refused(
        &c,
        DET,
        &det(5, NULL, "private_text", "live", NULL, NULL, NULL, NULL, t("maybe")),
        "CHECK constraint failed",
    );
    refused(&c, DET, &det(6, NULL, "private_text", "live", NULL, NULL, NULL, i(1), NULL), "detections.sitemap_seq");
    // owner_h and owner_via are set together, and owner_via is one of three.
    refused(&c, DET, &det(7, NULL, "private_text", "live", b(70), NULL, NULL, NULL, NULL), "CHECK constraint failed");
    refused(
        &c,
        DET,
        &det(8, NULL, "private_text", "live", NULL, t("created"), NULL, NULL, NULL),
        "CHECK constraint failed",
    );
    refused(
        &c,
        DET,
        &det(9, NULL, "private_text", "live", b(90), t("stolen"), NULL, NULL, NULL),
        "CHECK constraint failed",
    );
    run(&c, DET, &det(10, NULL, "private_text", "live", b(100), t("created"), b(10), NULL, NULL)).unwrap();
    // One share per scan.
    refused(&c, DET, &det(11, NULL, "private_text", "live", NULL, NULL, b(10), NULL, NULL), "detections.seal_nonce_h");
    // Public pages have no seal nonce: NULLs never collide.
    run(&c, DET, &det(12, t("abcdefg"), "public_url", "pending", NULL, NULL, NULL, NULL, NULL)).unwrap();
    run(&c, DET, &det(13, t("hjkmnpq"), "public_url", "failed", NULL, NULL, NULL, NULL, NULL)).unwrap();
    // Every kind, platform, state and owner_via in the sets is accepted.
    let mut n = 100u8;
    for k in Kind::STORED {
        for s in DetState::ALL {
            n += 1;
            run(&c, DET, &det(n, NULL, k.as_str(), s.as_str(), NULL, NULL, NULL, NULL, NULL)).unwrap();
        }
    }
    for v in OwnerVia::ALL {
        n += 1;
        run(&c, DET, &det(n, NULL, "private_paid", "pending", b(n), t(v.as_str()), NULL, NULL, NULL)).unwrap();
    }
    // A claim sets owner_h and owner_via together, once.
    let claim = "UPDATE detections SET owner_h = ?, owner_via = 'claimed', state_v = state_v + 1 \
                 WHERE id_h = ? AND state IN ('live','pending') AND owner_h IS NULL";
    assert_eq!(run(&c, claim, &[b(200), b(1)]).unwrap(), 1);
    assert_eq!(run(&c, claim, &[b(201), b(1)]).unwrap(), 0);
    // An owner action matches the page and the owner.
    let release = "UPDATE detections SET owner_h = NULL, owner_via = NULL WHERE id_h = ? AND owner_h = ?";
    assert_eq!(run(&c, release, &[b(1), b(201)]).unwrap(), 0);
    assert_eq!(run(&c, release, &[b(1), b(200)]).unwrap(), 1);
    // Clearing only one of the pair is refused.
    refused(&c, "UPDATE detections SET owner_via = NULL WHERE id_h = ?", &[b(10)], "CHECK constraint failed");
}

#[test]
fn a_rerun_copy_inherits_expiry_and_family() {
    let c = db();
    run(
        &c,
        "INSERT INTO detections (id_h, kind, platform, state, created_at, updated_at, expires_at) \
         VALUES (?, 'private_text', 'paste', 'live', 1, 1, 5000)",
        &[b(1)],
    )
    .unwrap();
    let copy = "INSERT INTO detections (id_h, kind, platform, state, created_at, updated_at, expires_at, expiry_cap, copy_of) \
                SELECT ?, 'private_text', 'paste', 'live', ?, ?, o.expires_at, o.expires_at, coalesce(o.copy_of, o.id_h) \
                FROM detections o WHERE o.id_h = ? AND o.state = 'live' AND (o.expires_at IS NULL OR o.expires_at > ?)";
    assert_eq!(run(&c, copy, &[b(2), i(10), i(10), b(1), i(10)]).unwrap(), 1);
    // A copy of a copy names the same root.
    assert_eq!(run(&c, copy, &[b(3), i(11), i(11), b(2), i(11)]).unwrap(), 1);
    let (latest, root): (i64, Vec<u8>) = c
        .query_row("SELECT expiry_cap, copy_of FROM detections WHERE id_h = ?", [vec![3u8; 32]], |r| {
            Ok((r.get(0)?, r.get(1)?))
        })
        .unwrap();
    assert_eq!((latest, root), (5000, vec![1u8; 32]));
    // An expired or deleted original yields no copy.
    assert_eq!(run(&c, copy, &[b(4), i(6000), i(6000), b(1), i(6000)]).unwrap(), 0);
    run(&c, "UPDATE detections SET state = 'deleted' WHERE id_h = ?", &[b(1)]).unwrap();
    assert_eq!(run(&c, copy, &[b(5), i(12), i(12), b(1), i(12)]).unwrap(), 0);
}

#[test]
fn jobs_are_fenced_and_unique() {
    let c = db();
    run(
        &c,
        "INSERT INTO detections (id_h, kind, platform, state, created_at, updated_at, owner_h, owner_via) \
         VALUES (?, 'private_paid', 'paste', 'pending', 1, 1, ?, 'paid')",
        &[b(1), b(50)],
    )
    .unwrap();
    let job = "INSERT INTO jobs (det_id_h, owner_h, hold_ref, package_load, policy_v, sections_total, sections_done, \
               tokens_billed, state, created_at, updated_at) VALUES (?, ?, ?, 'l', 'long-v1', 3, 1, 900, ?, 1, 1)";
    refused(&c, job, &[b(9), b(50), t("h0"), t("running")], "FOREIGN KEY constraint failed");
    refused(&c, job, &[b(1), b(50), t("h1"), t("paused")], "CHECK constraint failed");
    run(&c, job, &[b(1), b(50), t("h1"), t("running")]).unwrap();
    run(
        &c,
        "INSERT INTO detections (id_h, kind, platform, state, created_at, updated_at, owner_h, owner_via) \
         VALUES (?, 'private_paid', 'paste', 'pending', 1, 1, ?, 'paid')",
        &[b(2), b(50)],
    )
    .unwrap();
    refused(&c, job, &[b(2), b(50), t("h1"), t("running")], "jobs.hold_ref");
    // CLAIM takes the lease once; RENEW and ADVANCE need the token.
    let claim = "UPDATE jobs SET lease_token = ?, lease_until = ? WHERE det_id_h = ? AND state = 'running' \
                 AND (lease_until IS NULL OR lease_until < ?)";
    assert_eq!(run(&c, claim, &[b(7), i(100), b(1), i(10)]).unwrap(), 1);
    assert_eq!(run(&c, claim, &[b(8), i(100), b(1), i(20)]).unwrap(), 0);
    let advance = "UPDATE jobs SET sections_done = ? WHERE det_id_h = ? AND lease_token = ? AND state = 'running' \
                   AND sections_done = ? - 1";
    assert_eq!(run(&c, advance, &[i(2), b(1), b(8), i(2)]).unwrap(), 0);
    assert_eq!(run(&c, advance, &[i(2), b(1), b(7), i(2)]).unwrap(), 1);
    assert_eq!(run(&c, advance, &[i(2), b(1), b(7), i(2)]).unwrap(), 0);
}

#[test]
fn cases_strikes_and_journal() {
    let c = db();
    let case = "INSERT INTO cases (case_id, status_h, kind, state, received_at, updated_at, notice_key) \
                VALUES (?, ?, ?, ?, 1, 1, 'c/x')";
    for k in CaseKind::ALL {
        run(&c, case, &[t(&format!("td-{k}")), b(1), t(k.as_str()), t("received")]).unwrap();
    }
    refused(&c, case, &[t("td-a"), b(1), t("spam"), t("received")], "CHECK constraint failed");
    refused(&c, case, &[t("td-b"), b(1), t("copyright"), t("open")], "CHECK constraint failed");
    refused(
        &c,
        case,
        &[t("td-copyright"), b(1), t("copyright"), t("received")],
        "UNIQUE constraint failed: cases.case_id",
    );
    let target = "INSERT INTO case_targets VALUES (?, ?)";
    run(&c, target, &[t("td-copyright"), b(1)]).unwrap();
    refused(&c, target, &[t("td-copyright"), b(1)], "UNIQUE constraint failed");
    let strike = "INSERT INTO strikes (owner_h, case_id, at, owner_via, review) VALUES (?, ?, 1, ?, ?)";
    run(&c, strike, &[b(1), t("td-copyright"), t("claimed"), NULL]).unwrap();
    refused(&c, strike, &[b(1), t("td-copyright"), t("created"), NULL], "UNIQUE constraint failed");
    refused(&c, strike, &[b(2), t("td-copyright"), t("found"), NULL], "CHECK constraint failed");
    refused(&c, strike, &[b(3), t("td-copyright"), t("claimed"), t("maybe")], "CHECK constraint failed");
    run(&c, strike, &[b(4), t("td-copyright"), t("claimed"), t("not_counted")]).unwrap();
    let op = "INSERT INTO op_journal (at, actor, op, signature, nonce_h) VALUES (1, 'operator:00', '{}', NULL, ?)";
    run(&c, op, &[b(5)]).unwrap();
    refused(&c, op, &[b(5)], "op_journal.nonce_h");
}

#[test]
fn examples_and_counters() {
    let c = db();
    let ex = "INSERT INTO examples (slot, source, public_slug, doc_key, journal_seq) VALUES (?, ?, ?, ?, 1)";
    run(&c, ex, &[i(1), t("synthetic"), NULL, t("x/examples/v1/1")]).unwrap();
    run(&c, ex, &[i(4), t("public"), t("k7m2xq9"), NULL]).unwrap();
    refused(&c, ex, &[i(0), t("synthetic"), NULL, t("x")], "CHECK constraint failed");
    refused(&c, ex, &[i(5), t("synthetic"), NULL, t("x")], "CHECK constraint failed");
    refused(&c, ex, &[i(2), t("feed"), NULL, t("x")], "CHECK constraint failed");
    refused(&c, ex, &[i(2), t("synthetic"), t("k7m2xq9"), t("x")], "CHECK constraint failed");
    refused(&c, ex, &[i(2), t("synthetic"), NULL, NULL], "CHECK constraint failed");
    refused(&c, ex, &[i(3), t("public"), NULL, NULL], "CHECK constraint failed");
    refused(&c, ex, &[i(1), t("synthetic"), NULL, t("y")], "UNIQUE constraint failed");
    // The gate's flush writes absolute values: a repeated flush changes nothing.
    let flush = "INSERT INTO hour_counters (hour, channel, package_load, outcome, n) VALUES ('2026-11-13T10', ?, 'l', 'scored', ?) \
                 ON CONFLICT (hour, channel, package_load, outcome) DO UPDATE SET n = excluded.n";
    run(&c, flush, &[t("free_text"), i(7)]).unwrap();
    run(&c, flush, &[t("free_text"), i(7)]).unwrap();
    run(&c, flush, &[t("free_text"), i(9)]).unwrap();
    let n: i64 = c.query_row("SELECT n FROM hour_counters", [], |r| r.get(0)).unwrap();
    assert_eq!(n, 9);
    let receipt = "INSERT INTO paid_receipts VALUES (?, '2026-11-13T10', 'paid_page', 'l', ?, 900, 1, 1)";
    run(&c, receipt, &[t("r1"), t("h1")]).unwrap();
    refused(&c, receipt, &[t("r2"), t("h1")], "paid_receipts.hold_ref");
    refused(&c, receipt, &[t("r1"), t("h2")], "paid_receipts.receipt_id");
    let cov = "INSERT INTO edge_coverage (route, hour, covered, journal_seq) VALUES ('score', '2026-11-13T10', 1, 1)";
    run(&c, cov, &[]).unwrap();
    refused(&c, cov, &[], "UNIQUE constraint failed");
}
