//! Removal cases and signed ops on the detect store's own migration
//! (in-memory SQLite, foreign keys on): the form's two steps, the automatic
//! first steps, the case states, strikes, quarantine, the X removal, the op
//! envelope's refusals and the journal.

mod common;

use std::cell::RefCell;
use std::collections::BTreeMap;

use common::{Harness, block_on};
use serde_json::{Value, json};
use slop_ninja_account_wire::{UserEntry, UserState, UsersPage};
use slop_ninja_detect_page::api::{AutoStep, RemovalRequest, ShareRequest};
use slop_ninja_detect_page::codes::{CaseKind, Expiry, RemovalStep};
use slop_ninja_detect_page::ids::UserId;
use slop_ninja_detect_page::origins::Origins;
use slop_ninja_detect_page::page::{Page, Run};
use slop_ninja_detect_page::queue::Job;
use slop_ninja_detect_page::refusal::RefusalCode;
use slop_ninja_detect_page::sn::Principal;
use slop_ninja_detect_worker::cases::{self, Accounts, RemovalAnswer};
use slop_ninja_detect_worker::db::{Io, IoError};
use slop_ninja_detect_worker::doc::{DOC_V, RunDoc, Serving, StoredResult};
use slop_ninja_detect_worker::keys::{self, Pepper, Peppers, StoreKeys};
use slop_ninja_detect_worker::ops::{self, OpCtx};
use slop_ninja_detect_worker::share::{Keys, Sealed, share};
use slop_ninja_detect_worker::{canon, reads};
use slop_ninja_signed_op as sop;

const DIGEST: &str = "e2536d12dff593d7b11895b3cbf6b10232601abfbebde248325afd707722b5e8";
const TEXT: &str = "Tomato notes\n\nWe planted the tomatoes late this year because the frost hung around until the \
second week of May. The rain barrel overflowed twice in June, so we moved it under the shed gutter.";
const CLAIMANT: &str = "Claimant Fixture Name";

#[derive(Default)]
struct FakeAccounts {
    users: RefCell<Vec<UserEntry>>,
    calls: std::cell::Cell<u32>,
}

impl Accounts for FakeAccounts {
    async fn users_page(&self, after: Option<UserId>, limit: u32) -> Result<UsersPage, IoError> {
        self.calls.set(self.calls.get() + 1);
        let all = self.users.borrow();
        let users: Vec<UserEntry> =
            all.iter().filter(|u| after.is_none_or(|a| u.user_id.0 > a.0)).take(limit as usize).copied().collect();
        let next = if users.len() == limit as usize { users.last().map(|u| u.user_id) } else { None };
        Ok(UsersPage { users, next })
    }
}

struct World {
    h: Harness,
    store: StoreKeys,
    peppers: Peppers,
    origins: Origins,
    serving: Serving,
    key: sop::SigningKey,
    accounts: FakeAccounts,
    nonce: std::cell::Cell<u8>,
}

impl World {
    fn new() -> World {
        let key = sop::SigningKey::from_bytes(&[9; 32]);
        let h = Harness::new();
        let pk = sop::OperatorKey::from_verifying_key(key.verifying_key());
        h.vars.borrow_mut().insert("DETECT_OP_KEY".into(), pk.hex());
        h.vars.borrow_mut().insert("OP_ENV".into(), "staging".into());
        World {
            h,
            store: StoreKeys::derive(&[7u8; 32]),
            peppers: Peppers::new(vec![Pepper::new(1, [3u8; 32])]).unwrap(),
            origins: Origins::from_vars("https://site.example", "https://s.site.example").unwrap(),
            serving: Serving {
                load: "load-new".into(),
                label: "span package d24".into(),
                published: DIGEST.into(),
                pending: true,
                scorer_build: "7030a86f".into(),
                evidence: "evidence-d24-web-v1".into(),
            },
            key,
            accounts: FakeAccounts::default(),
            nonce: std::cell::Cell::new(0),
        }
    }

    fn keys(&self) -> Keys<'_> {
        Keys { store: &self.store, peppers: &self.peppers }
    }

    fn signed(&self, target: sop::Target, env: sop::Env, name: &str, args: Value) -> sop::Wire {
        self.nonce.set(self.nonce.get() + 1);
        let now = self.h.now_ms();
        let op = sop::Op {
            target,
            env,
            issued_ms: now,
            expires_ms: now + 60_000,
            nonce: sop::Nonce([self.nonce.get(); 16]),
            op: name.into(),
            args: args.as_object().unwrap().clone(),
        };
        sop::sign(&op, &self.key).unwrap()
    }

    fn post(&self, body: &[u8]) -> Result<Value, RefusalCode> {
        let ctx = OpCtx { keys: self.keys(), origins: &self.origins, accounts: &self.accounts };
        block_on(ops::op(&self.h, &ctx, body))
    }

    fn op(&self, name: &str, args: Value) -> Result<Value, RefusalCode> {
        let w = self.signed(sop::Target::Detect, sop::Env::Staging, name, args);
        self.post(&serde_json::to_vec(&w).unwrap())
    }

    fn removal(
        &self,
        step: RemovalStep,
        track: CaseKind,
        target: &str,
        fields: &[(&str, &str)],
        who: Option<Principal>,
    ) -> Result<RemovalAnswer, RefusalCode> {
        let req = RemovalRequest {
            step,
            track,
            target: target.into(),
            fields: fields.iter().map(|(k, v)| ((*k).to_owned(), (*v).to_owned())).collect(),
        };
        block_on(cases::removal(&self.h, self.keys(), &self.origins, who, &req))
    }

    fn send(
        &self,
        track: CaseKind,
        target: &str,
        fields: &[(&str, &str)],
        who: Option<Principal>,
    ) -> (String, String, Option<AutoStep>) {
        match self.removal(RemovalStep::Send, track, target, fields, who).unwrap() {
            RemovalAnswer::Received(r) => (r.case_id, r.status_token, r.auto),
            other => panic!("{other:?}"),
        }
    }

    fn page(&self, slug: &str) -> Result<Page, RefusalCode> {
        block_on(reads::page(&self.h, self.keys(), &self.serving, slug))
    }

    fn text(&self, sql: &str) -> String {
        self.h.db.query_row(sql, [], |r| r.get(0)).unwrap()
    }
}

fn fixture(name: &str) -> Page {
    let p = format!("{}/../crates/detect-page/tests/fixtures/pages/{name}.json", env!("CARGO_MANIFEST_DIR"));
    serde_json::from_str(&std::fs::read_to_string(p).unwrap()).unwrap()
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

/// A live public web page (fixture 01's run and document) at `slug`, with
/// its own source and run; `run` shares another page's run instead.
fn web_page(w: &World, slug: &str, run: Option<i64>) -> (i64, i64) {
    let p = fixture("01-public-very-strong");
    let t = p.text.as_ref().unwrap();
    w.h.db
        .execute(
            "INSERT INTO sources (canon_key, canon_url, host, adapter, state, reputation, rank_list_v, fetched_at) \
VALUES (?1, ?2, 'garden-diary.example', 'web', 'ok', 'ranked', 'v1', 1795169040000)",
            rusqlite::params![
                keys::h(b"source_v1", slug.as_bytes()).to_vec(),
                format!("https://garden-diary.example/{slug}")
            ],
        )
        .unwrap();
    let source_id = w.h.db.last_insert_rowid();
    let run_id = match run {
        Some(r) => r,
        None => {
            w.h.db
                .execute(
                    "INSERT INTO runs (package_load, package_pub, package_label, policy_v, canon_v, display_v, evidence_id, \
band, words, total_words, cut, tokens, scored_at, doc_key, doc_bytes) VALUES ('load-new', ?1, 'span package d24', 'p', \
'canon_v1', 'display_v1', 'e', 'very_strong', 300, 300, 'none', 400, 1, 'r/x', 1)",
                    [DIGEST],
                )
                .unwrap();
            let run_id = w.h.db.last_insert_rowid();
            let mut result = stored(p.run.as_ref().unwrap(), "load-new");
            result.spans = p.run.as_ref().unwrap().spans.clone();
            let doc = RunDoc {
                v: DOC_V,
                body: t.body.clone().unwrap(),
                anchors: t.anchors.clone(),
                further: t.further.clone(),
                page_words: t.page_words.unwrap_or(0),
                page_hash: p.source.as_ref().and_then(|s| s.page_hash.clone()),
                result_full: result,
            };
            block_on(w.h.doc_put(&format!("r/{run_id}"), doc.encode())).unwrap();
            run_id
        }
    };
    w.h.db
        .execute(
            "INSERT INTO detections (id_h, public_slug, kind, platform, state, created_at, updated_at, source_id, run_id, \
package_load, sitemap_seq) VALUES (?1, ?2, 'public_url', 'web', 'live', 1795169040000, 1795169040000, ?3, ?4, 'load-new', ?5)",
            rusqlite::params![keys::public_id_h(slug).to_vec(), slug, source_id, run_id, source_id],
        )
        .unwrap();
    (source_id, run_id)
}

/// A live X page about post `id` at `slug`.
fn x_page(w: &World, slug: &str, id: u64) -> i64 {
    let p = fixture("29-x-post-short");
    let key = cases::x_canon_key(id);
    w.h.db
        .execute(
            "INSERT INTO sources (canon_key, canon_url, host, adapter, state, reputation, rank_list_v, fetched_at) \
VALUES (?1, ?2, 'x.com', 'x', 'ok', 'ranked', 'x', 1795169040000) ON CONFLICT (canon_key) DO NOTHING",
            rusqlite::params![key.to_vec(), format!("https://x.com/i/status/{id}")],
        )
        .unwrap();
    let source_id: i64 =
        w.h.db.query_row("SELECT source_id FROM sources WHERE canon_key = ?1", [key.to_vec()], |r| r.get(0)).unwrap();
    w.h.db
        .execute(
            "INSERT INTO runs (package_load, package_pub, package_label, policy_v, canon_v, extract_v, display_v, \
evidence_id, band, words, total_words, cut, tokens, scored_at, doc_key, doc_bytes) VALUES ('load-new', ?1, \
'span package d24', 'p', 'canon_v1', 'x_text_v1', 'display_v1', 'e', 'strong', 40, 40, 'none', 60, 1, 'r/x', 1)",
            [DIGEST],
        )
        .unwrap();
    let run_id = w.h.db.last_insert_rowid();
    let mut result = stored(p.run.as_ref().unwrap(), "load-new");
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
    block_on(w.h.doc_put(&format!("r/{run_id}"), doc.encode())).unwrap();
    w.h.db
        .execute(
            "INSERT INTO detections (id_h, public_slug, kind, platform, state, created_at, updated_at, source_id, run_id, \
package_load) VALUES (?1, ?2, 'public_url', 'x', 'live', 1795169040000, 1795169040000, ?3, ?4, 'load-new')",
            rusqlite::params![keys::public_id_h(slug).to_vec(), slug, source_id, run_id],
        )
        .unwrap();
    run_id
}

fn principal(n: u8) -> Principal {
    Principal { user_id: UserId([n; 16]), auth_time_ms: 1, flags: 0 }
}

/// A secret page (Get a link), saved by `who` when given.
fn secret_page(w: &World, who: Option<Principal>) -> (String, String) {
    let mut result = stored(fixture("15-secret-text").run.as_ref().unwrap(), "load-new");
    result.spans.clear();
    let mut nonce = vec![0u8; 12];
    w.h.fill(&mut nonce);
    let sealed = Sealed { text_sha256: canon::text_sha256(TEXT), nonce, result };
    let req =
        ShareRequest { text: TEXT.into(), seal: "s".into(), expiry: Expiry::Keep, save: who.is_some(), source: None };
    let a = block_on(share(&w.h, w.keys(), &req, Some(sealed), who)).unwrap();
    (a.slug, a.manage)
}

fn copyright(scope: &'static str) -> Vec<(&'static str, &'static str)> {
    vec![
        ("work", "https://writer.example/essay"),
        ("scope", scope),
        ("name", CLAIMANT),
        ("email", "claimant@writer.invalid"),
        ("address", "1 Fixture Lane"),
        ("good_faith", "yes"),
        ("accurate", "yes"),
        ("signature", CLAIMANT),
    ]
}

fn state(w: &World, slug: &str, secret: bool) -> String {
    let id = if secret { w.peppers.current().id_h(slug) } else { keys::public_id_h(slug) };
    w.h.db.query_row("SELECT state FROM detections WHERE id_h = ?1", [id.to_vec()], |r| r.get(0)).unwrap()
}

fn queued(w: &World, f: impl Fn(&Job) -> bool) -> usize {
    w.h.queue.borrow().iter().filter(|m| f(&m.job)).count()
}

// --------------------------------------------------------------- the form

#[test]
fn the_review_checks_fields_and_target_and_writes_nothing() {
    let w = World::new();
    web_page(&w, "k7m2xq9", None);
    let before = w.h.dump();
    // Missing fields, a check not ticked, a select out of its choices.
    let mut f = copyright("everything");
    f.retain(|(k, _)| *k != "signature");
    f.retain(|(k, _)| *k != "accurate");
    f.push(("accurate", "no"));
    let RemovalAnswer::Review(r) = w.removal(RemovalStep::Review, CaseKind::Copyright, "k7m2xq9", &f, None).unwrap()
    else {
        panic!()
    };
    assert!(!r.complete);
    assert_eq!(r.missing, vec!["scope", "accurate", "signature"]);
    // The target as a slug, a folded slug and a link with a query and fragment.
    for t in ["k7m2xq9", "K7M2XQ9", "https://site.example/d/k7m2xq9?ref=1#x", "https://site.example/d/k7m2xq9/"] {
        let RemovalAnswer::Review(r) =
            w.removal(RemovalStep::Review, CaseKind::Copyright, t, &copyright("excerpt"), None).unwrap()
        else {
            panic!()
        };
        assert!(r.complete, "{t}: {r:?}");
    }
    // A link elsewhere, an unknown page, nothing.
    for t in ["https://elsewhere.example/d/k7m2xq9", "zzzzzzz", "", "https://site.example.evil/d/k7m2xq9"] {
        let RemovalAnswer::Review(r) =
            w.removal(RemovalStep::Review, CaseKind::Copyright, t, &copyright("excerpt"), None).unwrap()
        else {
            panic!()
        };
        assert_eq!(r.missing, vec!["target"], "{t}");
    }
    // Unknown fields, control characters, the x_request kind: refused.
    let mut odd = copyright("excerpt");
    odd.push(("nickname", "x"));
    assert_eq!(
        w.removal(RemovalStep::Review, CaseKind::Copyright, "k7m2xq9", &odd, None).err(),
        Some(RefusalCode::MalformedRequest)
    );
    let ctl = [("what", "bad\u{7}text")];
    assert_eq!(
        w.removal(RemovalStep::Review, CaseKind::WrongResult, "k7m2xq9", &ctl, None).err(),
        Some(RefusalCode::MalformedRequest)
    );
    assert_eq!(
        w.removal(RemovalStep::Review, CaseKind::XRequest, "k7m2xq9", &[], None).err(),
        Some(RefusalCode::MalformedRequest)
    );
    let big = "x".repeat(cases::NOTICE_FIELD_BYTES + 1);
    assert_eq!(
        w.removal(RemovalStep::Review, CaseKind::WrongResult, "k7m2xq9", &[("what", big.as_str())], None).err(),
        Some(RefusalCode::TooLarge)
    );
    // An incomplete send makes nothing.
    assert_eq!(
        w.removal(RemovalStep::Send, CaseKind::Copyright, "k7m2xq9", &f, None).err(),
        Some(RefusalCode::RemovalIncomplete)
    );
    assert_eq!(w.h.dump(), before, "review and refusals write nothing");
    assert!(w.h.queue.borrow().is_empty());
}

#[test]
fn a_copyright_notice_withholds_the_excerpt_at_once_and_keeps_the_notice_encrypted() {
    let w = World::new();
    web_page(&w, "k7m2xq9", None);
    let (case_id, token, auto) =
        w.send(CaseKind::Copyright, "https://site.example/d/k7m2xq9", &copyright("excerpt"), Some(principal(4)));
    assert_eq!(auto, Some(AutoStep::ExcerptWithheld));
    assert!(cases::is_case_id(&case_id), "{case_id}");
    assert_eq!(state(&w, "k7m2xq9", false), "excerpt_withheld");
    // The page shows its score and the withheld record, not its text.
    let p = w.page("k7m2xq9").unwrap();
    assert_eq!(p.state.as_str(), "excerpt_withheld");
    assert_eq!(p.removal.as_ref().unwrap().scope.as_str(), "excerpt");
    assert!(p.text.as_ref().unwrap().body.is_none());
    // D1 holds no notice text, address or token; R2 holds the notice sealed.
    let dump = w.h.dump();
    for s in [CLAIMANT, "claimant@writer.invalid", "writer.example/essay", token.as_str()] {
        assert!(!dump.contains(s), "D1 holds {s}");
    }
    let sealed = w.h.doc(&format!("c/{case_id}")).unwrap();
    assert!(!String::from_utf8_lossy(&sealed).contains(CLAIMANT));
    let plain = keys::open_with(w.store.cases_key(), slop_ninja_detect_worker::queue::CASE_AAD, &sealed).unwrap();
    let notice: cases::Notice = serde_json::from_slice(&plain).unwrap();
    assert_eq!(notice.fields["name"], CLAIMANT);
    assert_eq!(notice.target, "https://site.example/d/k7m2xq9");
    assert!(notice.signed_in);
    let submitter = w.text(&format!("SELECT hex(submitter_h) FROM cases WHERE case_id = '{case_id}'"));
    assert_eq!(submitter.to_ascii_lowercase(), hex::encode(w.store.owner_h(&[4; 16])));
    // The legal notice is queued; the status page answers the state only.
    assert_eq!(
        queued(&w, |j| matches!(j, Job::LegalNotice { case_id: c, kind: CaseKind::Copyright } if *c == case_id)),
        1
    );
    let s = block_on(cases::removal_status(&w.h, &token)).unwrap();
    assert_eq!((s.case_id.as_str(), s.state.as_str()), (case_id.as_str(), "received"));
    assert_eq!(block_on(cases::removal_status(&w.h, &"0".repeat(26))).err(), Some(RefusalCode::StatusUnknown));
    assert_eq!(block_on(cases::removal_status(&w.h, "short")).err(), Some(RefusalCode::StatusUnknown));
    // The legal stream carries the notice to the relay.
    let msg = w.h.queue.borrow().iter().find(|m| matches!(m.job, Job::LegalNotice { .. })).cloned().unwrap();
    assert_eq!(
        block_on(slop_ninja_detect_worker::queue::consume(&w.h, &w.store, &msg)),
        slop_ninja_detect_worker::queue::Ack::Done
    );
    let body: Value = serde_json::from_slice(&w.h.relay.borrow()[0]).unwrap();
    assert_eq!((body["case_id"].as_str(), body["targets"][0].as_str()), (Some(case_id.as_str()), Some("k7m2xq9")));
    assert_eq!(body["notice"]["fields"]["name"], CLAIMANT);
    // A rejected notice returns the page as it was, without its record.
    let r = w.op("reject", json!({"case_id": case_id, "reason": "not a copyright work"})).unwrap();
    assert_eq!(r["pages"], 1);
    assert_eq!(state(&w, "k7m2xq9", false), "live");
    assert_eq!(w.h.count("SELECT count(*) FROM removals"), 0);
    assert!(w.page("k7m2xq9").unwrap().text.unwrap().body.is_some());
    assert_eq!(block_on(cases::removal_status(&w.h, &token)).unwrap().state.as_str(), "rejected");
    // A rejected case cannot be rejected again; it can be closed.
    assert_eq!(w.op("reject", json!({"case_id": case_id, "reason": "again"})).err(), Some(RefusalCode::OpMalformed));
    assert!(w.op("close_case", json!({"case_id": case_id, "reason": "done"})).is_ok());
}

#[test]
fn accepting_withholds_every_page_of_the_run_and_rewrites_its_document() {
    let w = World::new();
    let (_, run) = web_page(&w, "k7m2xq9", None);
    web_page(&w, "b4n8r2t", Some(run));
    let (case_id, _, _) = w.send(CaseKind::Copyright, "k7m2xq9", &copyright("excerpt"), None);
    let a =
        w.op("withhold_excerpt", json!({"case_id": case_id, "jurisdiction": "US", "reason": "valid notice"})).unwrap();
    assert_eq!((a["ok"].clone(), a["op"].clone()), (json!(true), json!("withhold_excerpt")));
    assert!(a["seq"].as_u64().is_some());
    // The other page of the same text is withheld too, each with its record.
    assert_eq!(state(&w, "b4n8r2t", false), "excerpt_withheld");
    assert_eq!(w.h.count("SELECT count(*) FROM removals WHERE scope = 'excerpt'"), 2);
    let doc = RunDoc::decode(&w.h.doc(&format!("r/{run}")).unwrap()).unwrap();
    assert!(cases::is_withheld_doc(&doc) && doc.result_full.band.is_some(), "the text goes, the result stays");
    assert!(w.page("k7m2xq9").is_ok() && w.page("b4n8r2t").is_ok());
    assert_eq!(w.text(&format!("SELECT state FROM cases WHERE case_id = '{case_id}'")), "accepted");
    // A restore cannot bring the withheld text back: the case's page
    // returns failed (`report_gone`), saying to check the link again or
    // paste the text; no fetch is queued behind the reader's back.
    w.h.exec(&format!("UPDATE runs SET content_key = x'0102' WHERE run_id = {run}"));
    let r = w.op("restore", json!({"case_id": case_id, "reason": "counter evidence"})).unwrap();
    assert_eq!((r["pages"].clone(), r["report_gone"].clone(), r.get("refetch")), (json!(1), json!(1), None), "{r}");
    assert_eq!(queued(&w, |j| matches!(j, Job::RerunOp { .. })), 0);
    let p = w.page("k7m2xq9").unwrap();
    p.check().unwrap();
    assert_eq!((p.state.as_str(), p.failure.map(|f| f.code)), ("failed", Some(RefusalCode::ReportGone)));
    assert_eq!(p.source.as_ref().unwrap().host, "garden-diary.example", "its domain");
    assert!(p.run.is_none() && p.text.is_none());
    assert!(queued(&w, |j| matches!(j, Job::Purge { urls } if urls.iter().any(|u| u.contains("k7m2xq9")))) >= 1);
    // The other page of the run (withheld by the acceptance, not a target
    // of the case) keeps its state, and with it the run and its text-less
    // document; the run's reuse key is gone, so the same text is scored
    // afresh.
    assert_eq!(state(&w, "b4n8r2t", false), "excerpt_withheld");
    assert_eq!(w.h.count(&format!("SELECT count(*) FROM runs WHERE run_id = {run} AND content_key IS NULL")), 1);
    assert!(w.h.doc(&format!("r/{run}")).is_some());
    assert_eq!(w.text(&format!("SELECT state FROM cases WHERE case_id = '{case_id}'")), "restored");
}

/// A wrong-result report re-checks a shown result only: a live page queues
/// its source's re-check, while a pending or failed page (which names only
/// its domain-only source, no page to fetch) gets no automatic step.
#[test]
fn a_wrong_result_report_rechecks_a_shown_result_only() {
    let w = World::new();
    web_page(&w, "k7m2xq9", None);
    web_page(&w, "b4n8r2t", None);
    w.h.exec(
        "UPDATE detections SET state = 'failed', refusal = 'fetch_failed', run_id = NULL WHERE public_slug = 'b4n8r2t'",
    );
    let what = [("what", "the score is wrong")];
    let (_, _, auto) = w.send(CaseKind::WrongResult, "k7m2xq9", &what, None);
    assert_eq!(auto, Some(AutoStep::Recheck));
    assert_eq!(queued(&w, |j| matches!(j, Job::RerunOp { .. })), 1);
    for st in ["failed", "pending"] {
        w.h.exec(&format!("UPDATE detections SET state = '{st}' WHERE public_slug = 'b4n8r2t'"));
        let (_, _, auto) = w.send(CaseKind::WrongResult, "b4n8r2t", &what, None);
        assert_eq!(auto, None, "{st}");
    }
    assert_eq!(queued(&w, |j| matches!(j, Job::RerunOp { .. })), 1);
}

#[test]
fn a_removed_public_page_answers_451_and_returns_while_its_document_is_kept() {
    let w = World::new();
    let (_, run) = web_page(&w, "k7m2xq9", None);
    let (case_id, _, _) = w.send(
        CaseKind::PersonalData,
        "k7m2xq9",
        &[("name", "N"), ("email", "n@e.invalid"), ("data", "my name")],
        None,
    );
    assert_eq!(state(&w, "k7m2xq9", false), "live", "personal data waits for the operator");
    let r = w.op("remove", json!({"case_id": case_id, "jurisdiction": "EU", "reason": "GDPR erasure"})).unwrap();
    assert_eq!(r["pages"], 1);
    assert_eq!(w.page("k7m2xq9").unwrap().state.as_str(), "removed");
    let (basis, j): (String, String) =
        w.h.db.query_row("SELECT basis, jurisdiction FROM removals", [], |r| Ok((r.get(0)?, r.get(1)?))).unwrap();
    assert_eq!((basis.as_str(), j.as_str()), ("personal_data", "EU"));
    assert!(w.h.doc(&format!("r/{run}")).is_none(), "no other page reads the document");
    // Restored with its document gone: failed (`report_gone`), never live
    // with no document and never a silent fetch; the link can be checked
    // again.
    let r = w.op("restore", json!({"case_id": case_id, "reason": "mistake"})).unwrap();
    assert_eq!((r["pages"].clone(), r["report_gone"].clone()), (json!(1), json!(1)), "{r}");
    assert_eq!(queued(&w, |j| matches!(j, Job::RerunOp { .. })), 0);
    let p = w.page("k7m2xq9").unwrap();
    p.check().unwrap();
    assert_eq!((p.state.as_str(), p.failure.map(|f| f.code)), ("failed", Some(RefusalCode::ReportGone)));
    assert_eq!(w.h.count(&format!("SELECT count(*) FROM runs WHERE run_id = {run}")), 0, "the run goes");
    assert_eq!(w.h.count("SELECT count(*) FROM detections WHERE state = 'live' AND run_id IS NULL"), 0);
    // A restore outside a case of a page whose document went: failed too.
    let (_, run3) = web_page(&w, "d6q8t4w", None);
    assert_eq!(w.op("remove", json!({"slug": "d6q8t4w", "reason": "abuse"})).unwrap()["pages"], 1);
    assert!(w.h.doc(&format!("r/{run3}")).is_none());
    let r = w.op("restore", json!({"slug": "d6q8t4w", "reason": "lifted"})).unwrap();
    assert_eq!((r["pages"].clone(), r["report_gone"].clone()), (json!(1), json!(1)), "{r}");
    assert_eq!(state(&w, "d6q8t4w", false), "failed");
    assert_eq!(w.text("SELECT refusal FROM detections WHERE public_slug = 'd6q8t4w'"), "report_gone");
    assert_eq!(w.h.count(&format!("SELECT count(*) FROM runs WHERE run_id = {run3}")), 0);
    // Restoring it again changes nothing.
    assert_eq!(w.op("restore", json!({"slug": "d6q8t4w", "reason": "again"})).unwrap()["pages"], 0);
    // A page removed outside a case, with its document kept, returns at once.
    let (_, run2) = web_page(&w, "b4n8r2t", None);
    web_page(&w, "c5p9s3v", Some(run2));
    assert_eq!(w.op("remove", json!({"slug": "b4n8r2t", "reason": "abuse"})).unwrap()["pages"], 1);
    // Removed again: journaled, and no second record.
    assert_eq!(w.op("remove", json!({"slug": "b4n8r2t", "reason": "again"})).unwrap()["pages"], 0);
    assert_eq!(w.h.count("SELECT count(*) FROM removals WHERE public_slug = 'b4n8r2t'"), 1);
    assert!(w.h.doc(&format!("r/{run2}")).is_some(), "another page still reads it");
    assert_eq!(w.op("restore", json!({"slug": "b4n8r2t", "reason": "lifted"})).unwrap()["pages"], 1);
    assert_eq!(state(&w, "b4n8r2t", false), "live");
    // A secret slug, both forms, neither form: refused.
    for args in [
        json!({"slug": "a".repeat(20), "reason": "r"}),
        json!({"case_id": case_id, "slug": "b4n8r2t", "reason": "r"}),
        json!({"reason": "r"}),
    ] {
        assert_eq!(w.op("remove", args).err(), Some(RefusalCode::OpMalformed));
    }
}

/// The page a new check of a fixture page's source answers instead of
/// fetching: its newest page under a legal state (the URL check's own
/// `SELECT_KNOWN`), if any.
fn blocked(w: &World, slug: &str) -> Option<String> {
    w.h.db
        .query_row(
            slop_ninja_detect_worker::url_flow::SELECT_KNOWN,
            rusqlite::params![keys::h(b"source_v1", slug.as_bytes()).to_vec(), vec![0u8; 32], "load-new"],
            |r| r.get::<_, Option<String>>("blocked_slug"),
        )
        .unwrap()
}

fn case_state(w: &World, case_id: &str) -> String {
    w.text(&format!("SELECT state FROM cases WHERE case_id = '{case_id}'"))
}

/// Two cases on one page: restoring one never lifts the other's removal or
/// withhold. The page stays as it is (`on_hold`), with its run and document,
/// its link still answers the held page, and it returns with the last
/// restore of the cases holding it.
#[test]
fn a_restore_leaves_a_page_another_case_still_holds() {
    let w = World::new();
    let illegal = [("category", "other"), ("explanation", "it is illegal"), ("good_faith", "yes")];
    // Copyright case B withholds the page (accepted, its text rewritten
    // away); illegal-content case A then removes it, and its run document
    // with it.
    let (_, run) = web_page(&w, "k7m2xq9", None);
    let (b, _, auto) = w.send(CaseKind::Copyright, "k7m2xq9", &copyright("excerpt"), None);
    assert_eq!(auto, Some(AutoStep::ExcerptWithheld));
    w.op("withhold_excerpt", json!({"case_id": b, "reason": "valid notice"})).unwrap();
    w.h.advance(1000);
    let (a, _, auto) = w.send(CaseKind::Illegal, "k7m2xq9", &illegal, None);
    assert_eq!(auto, None);
    assert_eq!(w.op("remove", json!({"case_id": a, "reason": "illegal"})).unwrap()["pages"], 1);
    assert!(w.h.doc(&format!("r/{run}")).is_none());
    w.h.exec(&format!("UPDATE runs SET content_key = x'0102' WHERE run_id = {run}"));
    // Restoring B leaves the page removed under A: no state change, no
    // `report_gone`, the run kept with its reuse key, nothing fetched.
    let r = w.op("restore", json!({"case_id": b, "reason": "notice withdrawn"})).unwrap();
    assert_eq!((r["pages"].clone(), r["on_hold"].clone(), r.get("report_gone")), (json!(0), json!(1), None), "{r}");
    assert_eq!(case_state(&w, &b), "restored");
    assert_eq!(state(&w, "k7m2xq9", false), "removed");
    assert_eq!(
        w.h.count(&format!("SELECT count(*) FROM detections WHERE public_slug = 'k7m2xq9' AND run_id = {run}")),
        1
    );
    assert_eq!(w.h.count(&format!("SELECT count(*) FROM runs WHERE run_id = {run} AND content_key IS NOT NULL")), 1);
    assert_eq!(queued(&w, |j| matches!(j, Job::RerunOp { .. })), 0);
    assert_eq!(blocked(&w, "k7m2xq9").as_deref(), Some("k7m2xq9"), "the link still answers the removed page");
    // Nor does a restore by slug lift an open case's removal.
    let r = w.op("restore", json!({"slug": "k7m2xq9", "reason": "lifted"})).unwrap();
    assert_eq!((r["pages"].clone(), r["on_hold"].clone()), (json!(0), json!(1)), "{r}");
    assert_eq!(state(&w, "k7m2xq9", false), "removed");
    // Restoring A, the last case holding it, returns the page failed.
    let r = w.op("restore", json!({"case_id": a, "reason": "mistake"})).unwrap();
    assert_eq!((r["pages"].clone(), r["report_gone"].clone(), r.get("on_hold")), (json!(1), json!(1), None), "{r}");
    assert_eq!(state(&w, "k7m2xq9", false), "failed");
    assert_eq!(w.h.count(&format!("SELECT count(*) FROM runs WHERE run_id = {run}")), 0);
    assert_eq!(blocked(&w, "k7m2xq9"), None);

    // Two copyright cases on one page: C1 withheld it at intake, C2 (which
    // found it withheld) is accepted. A third notice that also found it
    // withheld is rejected: it had nothing to undo.
    let (_, run2) = web_page(&w, "b4n8r2t", None);
    w.h.advance(1000);
    let (c1, _, auto) = w.send(CaseKind::Copyright, "b4n8r2t", &copyright("excerpt"), None);
    assert_eq!(auto, Some(AutoStep::ExcerptWithheld));
    w.h.advance(1000);
    let (c2, _, auto) = w.send(CaseKind::Copyright, "b4n8r2t", &copyright("excerpt"), None);
    assert_eq!(auto, None);
    w.op("withhold_excerpt", json!({"case_id": c2, "reason": "valid notice"})).unwrap();
    w.h.advance(1000);
    let (c3, _, _) = w.send(CaseKind::Copyright, "b4n8r2t", &copyright("excerpt"), None);
    let r = w.op("reject", json!({"case_id": c3, "reason": "duplicate"})).unwrap();
    assert_eq!((r["pages"].clone(), r.get("on_hold")), (json!(0), None), "{r}");
    assert_eq!(state(&w, "b4n8r2t", false), "excerpt_withheld");
    // Rejecting C1 leaves C2's withhold, and C1's record with it.
    let r = w.op("reject", json!({"case_id": c1, "reason": "not a copyright work"})).unwrap();
    assert_eq!((r["pages"].clone(), r["on_hold"].clone()), (json!(0), json!(1)), "{r}");
    assert_eq!(case_state(&w, &c1), "rejected");
    assert_eq!(state(&w, "b4n8r2t", false), "excerpt_withheld");
    assert_eq!(w.h.count("SELECT count(*) FROM removals WHERE public_slug = 'b4n8r2t'"), 1);
    let p = w.page("b4n8r2t").unwrap();
    p.check().unwrap();
    assert_eq!(p.state.as_str(), "excerpt_withheld");
    assert!(p.text.as_ref().unwrap().body.is_none(), "no excerpt comes back");
    assert_eq!(blocked(&w, "b4n8r2t").as_deref(), Some("b4n8r2t"));
    // A fresh notice, C4, restored while C2 stands: the page stays withheld.
    w.h.advance(1000);
    let (c4, _, _) = w.send(CaseKind::Copyright, "b4n8r2t", &copyright("excerpt"), None);
    let r = w.op("restore", json!({"case_id": c4, "reason": "withdrawn"})).unwrap();
    assert_eq!((r["pages"].clone(), r["on_hold"].clone()), (json!(0), json!(1)), "{r}");
    assert_eq!(state(&w, "b4n8r2t", false), "excerpt_withheld");
    assert!(w.h.doc(&format!("r/{run2}")).is_some());
    // Restoring C2 returns it: its text was rewritten away, so failed.
    let r = w.op("restore", json!({"case_id": c2, "reason": "counter evidence"})).unwrap();
    assert_eq!((r["pages"].clone(), r["report_gone"].clone()), (json!(1), json!(1)), "{r}");
    assert_eq!(state(&w, "b4n8r2t", false), "failed");
    assert_eq!(blocked(&w, "b4n8r2t"), None);

    // A copyright notice awaiting its decision holds the page its intake
    // withheld: removed and restored under another case, the page stays
    // removed. Once that notice is rejected (which leaves a removed page as
    // it is), `restore {slug}` returns it.
    web_page(&w, "c5p9s3v", None);
    w.h.advance(1000);
    let (d1, _, auto) = w.send(CaseKind::Copyright, "c5p9s3v", &copyright("excerpt"), None);
    assert_eq!(auto, Some(AutoStep::ExcerptWithheld));
    w.h.advance(1000);
    let (d2, _, _) = w.send(CaseKind::Illegal, "c5p9s3v", &illegal, None);
    w.op("remove", json!({"case_id": d2, "reason": "illegal"})).unwrap();
    let r = w.op("restore", json!({"case_id": d2, "reason": "mistake"})).unwrap();
    assert_eq!((r["pages"].clone(), r["on_hold"].clone()), (json!(0), json!(1)), "{r}");
    assert_eq!((state(&w, "c5p9s3v", false), case_state(&w, &d1)), ("removed".into(), "received".into()));
    let r = w.op("reject", json!({"case_id": d1, "reason": "not a copyright work"})).unwrap();
    assert_eq!((r["pages"].clone(), r.get("on_hold")), (json!(0), None), "{r}");
    assert_eq!(state(&w, "c5p9s3v", false), "removed");
    let r = w.op("restore", json!({"slug": "c5p9s3v", "reason": "lifted"})).unwrap();
    assert_eq!((r["pages"].clone(), r["report_gone"].clone()), (json!(1), json!(1)), "{r}");
    assert_eq!(state(&w, "c5p9s3v", false), "failed");

    // A reject never returns a page whose text an accepted withhold took,
    // even once that case is closed.
    web_page(&w, "h3k7m9q", None);
    w.h.advance(1000);
    let (q1, _, _) = w.send(CaseKind::Copyright, "h3k7m9q", &copyright("excerpt"), None);
    w.h.advance(1000);
    let (q2, _, _) = w.send(CaseKind::Copyright, "h3k7m9q", &copyright("excerpt"), None);
    w.op("withhold_excerpt", json!({"case_id": q2, "reason": "valid notice"})).unwrap();
    w.op("close_case", json!({"case_id": q2, "reason": "upheld"})).unwrap();
    let r = w.op("reject", json!({"case_id": q1, "reason": "not a copyright work"})).unwrap();
    assert_eq!((r["pages"].clone(), r["on_hold"].clone()), (json!(0), json!(1)), "{r}");
    assert_eq!(state(&w, "h3k7m9q", false), "excerpt_withheld");
    assert_eq!(blocked(&w, "h3k7m9q").as_deref(), Some("h3k7m9q"));
}

/// A case holding a page through its run, or once closed: a restore keeps
/// the page; `restore {slug}`, the operator's decision on the page itself,
/// returns it. A case that comes to hold the page between the restore's
/// read and its batch keeps it too, with its run and document.
#[test]
fn a_restore_keeps_a_page_held_through_its_run_or_by_a_closed_case() {
    let w = World::new();
    let personal = [("name", "N"), ("email", "n@e.invalid"), ("data", "my name")];
    // A closed removal: E1 removed the page and was closed; E2, sent while
    // the page was removed, is removed (nothing changes) and restored.
    let (_, run) = web_page(&w, "d6q8t4w", None);
    let (e1, _, _) = w.send(CaseKind::PersonalData, "d6q8t4w", &personal, None);
    w.op("remove", json!({"case_id": e1, "reason": "erasure"})).unwrap();
    w.op("close_case", json!({"case_id": e1, "reason": "upheld"})).unwrap();
    w.h.advance(1000);
    let (e2, _, auto) = w.send(CaseKind::Copyright, "d6q8t4w", &copyright("page"), None);
    assert_eq!(auto, None);
    w.op("remove", json!({"case_id": e2, "reason": "valid notice"})).unwrap();
    let r = w.op("restore", json!({"case_id": e2, "reason": "withdrawn"})).unwrap();
    assert_eq!((r["pages"].clone(), r["on_hold"].clone()), (json!(0), json!(1)), "{r}");
    assert_eq!(state(&w, "d6q8t4w", false), "removed");
    let r = w.op("restore", json!({"slug": "d6q8t4w", "reason": "lifted"})).unwrap();
    assert_eq!((r["pages"].clone(), r["report_gone"].clone()), (json!(1), json!(1)), "{r}");
    assert_eq!(state(&w, "d6q8t4w", false), "failed");
    assert_eq!(w.h.count(&format!("SELECT count(*) FROM runs WHERE run_id = {run}")), 0);
    // A case closed before a notice found the page live holds nothing for
    // that notice's restore.
    let (_, run2) = web_page(&w, "b4n8r2t", None);
    w.h.advance(1000);
    let (f1, _, _) = w.send(CaseKind::Copyright, "b4n8r2t", &copyright("excerpt"), None);
    w.op("reject", json!({"case_id": f1, "reason": "not a copyright work"})).unwrap();
    w.op("close_case", json!({"case_id": f1, "reason": "done"})).unwrap();
    assert_eq!(state(&w, "b4n8r2t", false), "live");
    w.h.advance(1000);
    let (f2, _, auto) = w.send(CaseKind::Copyright, "b4n8r2t", &copyright("excerpt"), None);
    assert_eq!(auto, Some(AutoStep::ExcerptWithheld));
    let r = w.op("restore", json!({"case_id": f2, "reason": "withdrawn"})).unwrap();
    assert_eq!((r["pages"].clone(), r.get("on_hold")), (json!(1), None), "{r}");
    assert_eq!(state(&w, "b4n8r2t", false), "live");
    assert!(w.h.doc(&format!("r/{run2}")).is_some());
    // Through its run: G1's accepted withhold covers the other page of the
    // same text; G2, sent on that page and restored, leaves it withheld.
    let (_, run3) = web_page(&w, "c5p9s3v", None);
    web_page(&w, "h3k7m9q", Some(run3));
    w.h.advance(1000);
    let (g1, _, _) = w.send(CaseKind::Copyright, "c5p9s3v", &copyright("excerpt"), None);
    w.op("withhold_excerpt", json!({"case_id": g1, "reason": "valid notice"})).unwrap();
    assert_eq!(state(&w, "h3k7m9q", false), "excerpt_withheld");
    w.h.advance(1000);
    let (g2, _, _) = w.send(CaseKind::Copyright, "h3k7m9q", &copyright("excerpt"), None);
    let r = w.op("restore", json!({"case_id": g2, "reason": "withdrawn"})).unwrap();
    assert_eq!((r["pages"].clone(), r["on_hold"].clone()), (json!(0), json!(1)), "{r}");
    assert_eq!(state(&w, "h3k7m9q", false), "excerpt_withheld");
    assert_eq!(w.h.count(&format!("SELECT count(*) FROM detections WHERE run_id = {run3}")), 2);
    // A hold that lands between the read and the batch: the page, its run
    // and its document stay.
    let (_, run4) = web_page(&w, "m2q8r4t", None);
    w.h.advance(1000);
    let (k1, _, _) = w.send(CaseKind::PersonalData, "m2q8r4t", &personal, None);
    w.op("remove", json!({"case_id": k1, "reason": "erasure"})).unwrap();
    w.h.exec(&format!("UPDATE runs SET content_key = x'0102' WHERE run_id = {run4}"));
    w.h.before("UPDATE detections SET state = 'failed'", |db| race_hold(db, "td-race0001", "m2q8r4t"));
    let r = w.op("restore", json!({"case_id": k1, "reason": "mistake"})).unwrap();
    assert_eq!((r["pages"].clone(), r["on_hold"].clone(), r.get("report_gone")), (json!(0), json!(1), None), "{r}");
    assert_eq!(state(&w, "m2q8r4t", false), "removed");
    assert_eq!(w.h.count(&format!("SELECT count(*) FROM runs WHERE run_id = {run4} AND content_key IS NOT NULL")), 1);
    assert_eq!(w.h.count(&format!("SELECT count(*) FROM detections WHERE run_id = {run4}")), 1);
    // The same for a page a restore would return live, and for a reject,
    // which also keeps its intake record.
    for (n, slug, op) in [(2, "r4t8m2q", "restore"), (3, "t6v2k8n", "reject")] {
        web_page(&w, slug, None);
        w.h.advance(1000);
        let (k, _, auto) = w.send(CaseKind::Copyright, slug, &copyright("excerpt"), None);
        assert_eq!(auto, Some(AutoStep::ExcerptWithheld));
        let id = format!("td-race000{n}");
        let s = slug.to_owned();
        w.h.before("UPDATE detections SET state = 'live'", move |db| race_hold(db, &id, &s));
        let r = w.op(op, json!({"case_id": k, "reason": "review done"})).unwrap();
        assert_eq!((r["pages"].clone(), r["on_hold"].clone()), (json!(0), json!(1)), "{op}: {r}");
        assert_eq!(state(&w, slug, false), "excerpt_withheld", "{op}");
        assert_eq!(w.h.count(&format!("SELECT count(*) FROM removals WHERE public_slug = '{slug}'")), 1, "{op}");
    }
}

/// The operator's hold that `remove {slug}` leaves on `slug`'s page.
fn op_hold(w: &World, slug: &str) -> bool {
    w.h.count(&format!("SELECT count(*) FROM detections WHERE public_slug = '{slug}' AND op_hold_at IS NOT NULL")) == 1
}

/// A removal made outside any case (`remove {slug}`) holds the page against
/// every restore by case: neither a notice that withheld the page at intake
/// (A) nor a case accepted on the page once it was removed (C) returns it.
/// The page stays removed with its run and reuse key, its link still answers
/// the removed page, and `restore {slug}` returns it. A case that never
/// accepted a removal never returns a removed page either.
#[test]
fn a_restore_by_case_never_returns_a_page_removed_outside_it() {
    let w = World::new();
    let personal = [("name", "N"), ("email", "n@e.invalid"), ("data", "my name")];
    // (A) Copyright notice B withholds the page at intake; the operator then
    // removes it by slug, and its run document goes.
    let (_, run) = web_page(&w, "k7m2xq9", None);
    w.h.advance(1000);
    let (b, _, auto) = w.send(CaseKind::Copyright, "k7m2xq9", &copyright("excerpt"), None);
    assert_eq!(auto, Some(AutoStep::ExcerptWithheld));
    assert_eq!(w.op("remove", json!({"slug": "k7m2xq9", "reason": "abuse"})).unwrap()["pages"], 1);
    assert!(w.h.doc(&format!("r/{run}")).is_none());
    assert!(op_hold(&w, "k7m2xq9"));
    assert_eq!(blocked(&w, "k7m2xq9").as_deref(), Some("k7m2xq9"));
    w.h.exec(&format!("UPDATE runs SET content_key = x'0102' WHERE run_id = {run}"));
    // Restoring B (a legal edge from `received`) leaves the page removed:
    // no state change, no `report_gone`, its run kept with its reuse key,
    // nothing fetched, and its link still answers the removed page.
    let r = w.op("restore", json!({"case_id": b, "reason": "notice withdrawn"})).unwrap();
    assert_eq!((r["pages"].clone(), r["on_hold"].clone(), r.get("report_gone")), (json!(0), json!(1), None), "{r}");
    assert_eq!(case_state(&w, &b), "restored");
    assert_eq!(state(&w, "k7m2xq9", false), "removed");
    assert_eq!(
        w.h.count(&format!("SELECT count(*) FROM detections WHERE public_slug = 'k7m2xq9' AND run_id = {run}")),
        1
    );
    assert_eq!(w.h.count(&format!("SELECT count(*) FROM runs WHERE run_id = {run} AND content_key IS NOT NULL")), 1);
    assert_eq!(queued(&w, |j| matches!(j, Job::RerunOp { .. })), 0);
    assert_eq!(blocked(&w, "k7m2xq9").as_deref(), Some("k7m2xq9"));
    // `restore {slug}` returns it, failed (its document went), and the hold
    // goes with it; the link is free.
    let r = w.op("restore", json!({"slug": "k7m2xq9", "reason": "lifted"})).unwrap();
    assert_eq!((r["pages"].clone(), r["report_gone"].clone()), (json!(1), json!(1)), "{r}");
    assert_eq!(state(&w, "k7m2xq9", false), "failed");
    assert!(!op_hold(&w, "k7m2xq9"));
    assert_eq!(w.h.count(&format!("SELECT count(*) FROM runs WHERE run_id = {run}")), 0);
    assert_eq!(blocked(&w, "k7m2xq9"), None);

    // (C) The operator removes a page by slug (another page of the same text
    // keeps the run document). Copyright notice C arrives later and gets no
    // automatic step; `remove {case_id: C}` removes nothing more but accepts
    // C. Restoring C leaves the page removed, though its report is kept.
    let (_, run2) = web_page(&w, "b4n8r2t", None);
    web_page(&w, "c5p9s3v", Some(run2));
    assert_eq!(w.op("remove", json!({"slug": "b4n8r2t", "reason": "abuse"})).unwrap()["pages"], 1);
    w.h.advance(1000);
    let (c, _, auto) = w.send(CaseKind::Copyright, "b4n8r2t", &copyright("page"), None);
    assert_eq!(auto, None);
    let r = w.op("remove", json!({"case_id": c, "reason": "valid notice"})).unwrap();
    assert_eq!((r["pages"].clone(), case_state(&w, &c)), (json!(0), "accepted".into()), "{r}");
    let r = w.op("restore", json!({"case_id": c, "reason": "counter evidence"})).unwrap();
    assert_eq!((r["pages"].clone(), r["on_hold"].clone(), r.get("report_gone")), (json!(0), json!(1), None), "{r}");
    assert_eq!(case_state(&w, &c), "restored");
    assert_eq!(state(&w, "b4n8r2t", false), "removed");
    assert!(w.h.doc(&format!("r/{run2}")).is_some());
    assert_eq!(blocked(&w, "b4n8r2t").as_deref(), Some("b4n8r2t"));
    let r = w.op("restore", json!({"slug": "b4n8r2t", "reason": "lifted"})).unwrap();
    assert_eq!((r["pages"].clone(), r.get("report_gone")), (json!(1), None), "{r}");
    assert_eq!(state(&w, "b4n8r2t", false), "live");
    assert!(!op_hold(&w, "b4n8r2t"));

    // The other order: a case removes the page, then the operator's
    // `remove {slug}` finds it removed (nothing changes, no second record)
    // and holds it; restoring the case leaves it removed.
    web_page(&w, "d6q8t4w", None);
    w.h.advance(1000);
    let (e, _, _) = w.send(CaseKind::PersonalData, "d6q8t4w", &personal, None);
    w.op("remove", json!({"case_id": e, "reason": "erasure"})).unwrap();
    assert!(!op_hold(&w, "d6q8t4w"));
    assert_eq!(w.op("remove", json!({"slug": "d6q8t4w", "reason": "abuse"})).unwrap()["pages"], 0);
    assert_eq!(w.h.count("SELECT count(*) FROM removals WHERE public_slug = 'd6q8t4w'"), 1);
    assert!(op_hold(&w, "d6q8t4w"));
    let r = w.op("restore", json!({"case_id": e, "reason": "mistake"})).unwrap();
    assert_eq!((r["pages"].clone(), r["on_hold"].clone()), (json!(0), json!(1)), "{r}");
    assert_eq!(state(&w, "d6q8t4w", false), "removed");
    let r = w.op("restore", json!({"slug": "d6q8t4w", "reason": "lifted"})).unwrap();
    assert_eq!((r["pages"].clone(), r["report_gone"].clone()), (json!(1), json!(1)), "{r}");
    assert_eq!(state(&w, "d6q8t4w", false), "failed");

    // A case that never accepted a removal (awaiting its decision, or
    // rejected) never returns a removed page, even with no other hold left:
    // F removes the page and is restored while notices H and H2 (sent once
    // the page was removed) hold it; H2 is rejected (which leaves a removed
    // page as it is). Restoring H, then H2, leaves it removed, and
    // `restore {slug}` returns it.
    web_page(&w, "h3k7m9q", None);
    w.h.advance(1000);
    let (f, _, _) = w.send(CaseKind::PersonalData, "h3k7m9q", &personal, None);
    w.op("remove", json!({"case_id": f, "reason": "erasure"})).unwrap();
    w.h.advance(1000);
    let (h, _, auto) = w.send(CaseKind::Copyright, "h3k7m9q", &copyright("page"), None);
    assert_eq!(auto, None);
    let (h2, _, _) = w.send(CaseKind::Copyright, "h3k7m9q", &copyright("page"), None);
    let r = w.op("restore", json!({"case_id": f, "reason": "mistake"})).unwrap();
    assert_eq!((r["pages"].clone(), r["on_hold"].clone()), (json!(0), json!(1)), "{r}");
    assert_eq!(w.op("reject", json!({"case_id": h2, "reason": "duplicate"})).unwrap()["pages"], 0);
    for (id, st) in [(&h, "restored"), (&h2, "rejected")] {
        let r = w.op("restore", json!({"case_id": id, "reason": "notice withdrawn"})).unwrap();
        assert_eq!((r["pages"].clone(), r["on_hold"].clone()), (json!(0), json!(1)), "{st}: {r}");
        assert_eq!((case_state(&w, id), state(&w, "h3k7m9q", false)), (st.into(), "removed".into()));
    }
    assert_eq!(blocked(&w, "h3k7m9q").as_deref(), Some("h3k7m9q"));
    let r = w.op("restore", json!({"slug": "h3k7m9q", "reason": "lifted"})).unwrap();
    assert_eq!((r["pages"].clone(), r["report_gone"].clone()), (json!(1), json!(1)), "{r}");
    assert_eq!(state(&w, "h3k7m9q", false), "failed");

    // The operator's hold landing between a restore's read and its batch:
    // the page, its run and its reuse key stay.
    let (_, run4) = web_page(&w, "m2q8r4t", None);
    w.h.advance(1000);
    let (k, _, _) = w.send(CaseKind::PersonalData, "m2q8r4t", &personal, None);
    w.op("remove", json!({"case_id": k, "reason": "erasure"})).unwrap();
    w.h.exec(&format!("UPDATE runs SET content_key = x'0102' WHERE run_id = {run4}"));
    w.h.before("UPDATE detections SET state = 'failed'", |db| {
        db.execute("UPDATE detections SET op_hold_at = 1 WHERE public_slug = 'm2q8r4t'", []).unwrap();
    });
    let r = w.op("restore", json!({"case_id": k, "reason": "mistake"})).unwrap();
    assert_eq!((r["pages"].clone(), r["on_hold"].clone(), r.get("report_gone")), (json!(0), json!(1), None), "{r}");
    assert_eq!(state(&w, "m2q8r4t", false), "removed");
    assert_eq!(w.h.count(&format!("SELECT count(*) FROM runs WHERE run_id = {run4} AND content_key IS NOT NULL")), 1);
    assert_eq!(w.h.count(&format!("SELECT count(*) FROM detections WHERE run_id = {run4}")), 1);
    // A page that leaves the state the restore read before its batch (here
    // withheld at intake, then removed) is left as it is: a notice awaiting
    // its decision returns its withheld page, never a removed one, whether
    // its report is kept (live) or not (failed).
    for (slug, gone) in [("r4t8m2q", false), ("t6v2k8n", true)] {
        let (_, run) = web_page(&w, slug, None);
        w.h.advance(1000);
        let (n, _, auto) = w.send(CaseKind::Copyright, slug, &copyright("excerpt"), None);
        assert_eq!(auto, Some(AutoStep::ExcerptWithheld));
        if gone {
            block_on(w.h.doc_delete(&[format!("r/{run}")])).unwrap();
        }
        let s = slug.to_owned();
        let needle =
            if gone { "UPDATE detections SET state = 'failed'" } else { "UPDATE detections SET state = 'live'" };
        w.h.before(needle, move |db| {
            db.execute("UPDATE detections SET state = 'removed' WHERE public_slug = ?1", [&s]).unwrap();
        });
        let r = w.op("restore", json!({"case_id": n, "reason": "notice withdrawn"})).unwrap();
        assert_eq!((r["pages"].clone(), r["on_hold"].clone()), (json!(0), json!(1)), "{slug}: {r}");
        assert_eq!(state(&w, slug, false), "removed", "{slug}");
        assert_eq!(w.h.count(&format!("SELECT count(*) FROM runs WHERE run_id = {run}")), 1, "{slug}");
    }
}

/// An accepted case on `slug`'s page, written as if it landed between an
/// op's read and its batch.
fn race_hold(db: &rusqlite::Connection, case_id: &str, slug: &str) {
    db.execute(
        "INSERT INTO cases (case_id, status_h, kind, state, received_at, updated_at, notice_key) \
VALUES (?1, ?2, 'illegal', 'accepted', 1, 1, 'c/' || ?1)",
        rusqlite::params![case_id, case_id.as_bytes()],
    )
    .unwrap();
    db.execute(
        "INSERT INTO case_targets (case_id, det_id_h) SELECT ?1, id_h FROM detections WHERE public_slug = ?2",
        [case_id, slug],
    )
    .unwrap();
}

#[test]
fn an_illegal_report_disables_the_family_and_a_removal_quarantines_it() {
    let w = World::new();
    let (slug, _) = secret_page(&w, Some(principal(5)));
    let id = w.peppers.current().id_h(&slug);
    // A re-run copy of the page.
    w.h.db
        .execute(
            "INSERT INTO detections (id_h, pepper_v, kind, platform, state, created_at, updated_at, copy_of) \
VALUES (?2, 1, 'private_text', 'paste', 'live', 1, 1, ?1)",
            [id.to_vec(), vec![0x77u8; 32]],
        )
        .unwrap();
    let copy_state = || w.text(&format!("SELECT state FROM detections WHERE id_h = x'{}'", "77".repeat(32)));
    let fields = [("category", "other"), ("explanation", "it is illegal"), ("good_faith", "yes")];
    let link = format!("https://s.site.example/{slug}#m=secret-manage");
    let (case_id, _, auto) = w.send(CaseKind::Illegal, &link, &fields, None);
    assert_eq!(auto, Some(AutoStep::Disabled));
    assert_eq!(state(&w, &slug, true), "disabled");
    assert_eq!(copy_state(), "disabled");
    assert_eq!(w.page(&slug).unwrap().state.as_str(), "disabled");
    // The notice keeps the page's link, never the manage fragment.
    let plain = keys::open_with(
        w.store.cases_key(),
        slop_ninja_detect_worker::queue::CASE_AAD,
        &w.h.doc(&format!("c/{case_id}")).unwrap(),
    )
    .unwrap();
    let s = String::from_utf8(plain).unwrap();
    assert!(s.contains(&slug) && !s.contains("secret-manage"), "{s}");
    // Removal: the documents move to quarantine and the page answers 451.
    let doc_before = w.h.doc(&format!("p/{}", hex::encode(id))).unwrap();
    let r = w.op("remove", json!({"case_id": case_id, "reason": "illegal"})).unwrap();
    assert_eq!(r["pages"], 2, "{r}");
    assert_eq!(state(&w, &slug, true), "removed");
    assert!(w.h.doc(&format!("p/{}", hex::encode(id))).is_none());
    let q = w.h.doc(&cases::quarantine_key(&case_id, &id)).unwrap();
    assert_ne!(q, doc_before, "wrapped again under K_cases");
    assert_eq!(w.h.count("SELECT count(*) FROM removals WHERE public_slug IS NULL"), 2);
    // The removal record counts secret pages only by month.
    let rec = block_on(reads::removals(&w.h)).unwrap();
    assert!(rec.entries.is_empty() && rec.secret_monthly.iter().map(|m| m.count).sum::<u32>() == 2);
    // Restored: the document comes back exactly, and the quarantine goes.
    let r = w.op("restore", json!({"case_id": case_id, "reason": "court order lifted"})).unwrap();
    assert_eq!(r["pages"], 1, "the copy had no document to put back: {r}");
    assert_eq!(state(&w, &slug, true), "live");
    assert_eq!(w.h.doc(&format!("p/{}", hex::encode(id))).unwrap(), doc_before);
    assert!(w.h.doc(&cases::quarantine_key(&case_id, &id)).is_none());
    assert_eq!(w.page(&slug).unwrap().state.as_str(), "live");
    // Removed again under a new case, then closed: the quarantine is deleted.
    let (case2, _, _) = w.send(CaseKind::Illegal, &slug, &fields, None);
    w.op("remove", json!({"case_id": case2, "reason": "illegal"})).unwrap();
    assert!(w.h.doc(&cases::quarantine_key(&case2, &id)).is_some());
    let c = w.op("close_case", json!({"case_id": case2, "reason": "upheld"})).unwrap();
    assert_eq!(c["quarantine_deleted"], 1);
    assert!(w.h.doc(&cases::quarantine_key(&case2, &id)).is_none());
    assert_eq!(w.op("close_case", json!({"case_id": case2, "reason": "again"})).err(), Some(RefusalCode::OpMalformed));
}

/// A paid page whose job never scored (pending, no document) disabled by an
/// illegal-content report, then rejected, or removed and restored: it
/// returns failed (`job_stopped`), never live without a document, and the
/// sweep cancels its job (its hold goes back). A live paid page comes back
/// live.
#[test]
fn a_paid_page_whose_job_never_scored_returns_failed_from_a_review() {
    let fields = [("category", "other"), ("explanation", "it is illegal"), ("good_faith", "yes")];
    for how in ["reject", "restore"] {
        let w = World::new();
        let (slug, _) = secret_page(&w, Some(principal(7)));
        let idx = hex::encode(w.peppers.current().id_h(&slug));
        // The page as a paid start leaves it: pending, its job running, no
        // document yet.
        w.h.exec(&format!(
            "UPDATE detections SET kind = 'private_paid', owner_via = 'paid', state = 'pending', doc_key = NULL, \
             doc_bytes = NULL WHERE id_h = x'{idx}'"
        ));
        block_on(w.h.doc_delete(&[format!("p/{idx}")])).unwrap();
        w.h.exec(&format!(
            "INSERT INTO jobs (det_id_h, owner_h, hold_ref, package_load, policy_v, sections_total, sections_done, \
             tokens_billed, state, created_at, updated_at) SELECT id_h, owner_h, 'h-{how}', 'load-new', 'long-v1', 3, 1, \
             900, 'running', 1, 1 FROM detections WHERE id_h = x'{idx}'"
        ));
        let (case_id, _, auto) = w.send(CaseKind::Illegal, &format!("https://s.site.example/{slug}"), &fields, None);
        assert_eq!(auto, Some(AutoStep::Disabled));
        assert_eq!(state(&w, &slug, true), "disabled");
        if how == "restore" {
            w.op("remove", json!({"case_id": case_id, "reason": "illegal"})).unwrap();
            assert_eq!(state(&w, &slug, true), "removed");
        }
        let r = w.op(how, json!({"case_id": case_id, "reason": "review done"})).unwrap();
        assert_eq!(r["pages"], 1, "{how}: {r}");
        assert_eq!(state(&w, &slug, true), "failed", "{how}");
        assert_eq!(w.text(&format!("SELECT refusal FROM detections WHERE id_h = x'{idx}'")), "job_stopped");
        assert_eq!(w.h.count("SELECT count(*) FROM detections WHERE state = 'live' AND doc_key IS NULL"), 0);
        let p = w.page(&slug).unwrap();
        p.check().unwrap();
        assert_eq!(p.failure.unwrap().code, RefusalCode::JobStopped);
        // The page no longer pends, so the sweep cancels its job and sends
        // its hold back through commit_hold.
        block_on(slop_ninja_detect_worker::paid::sweep(&w.h)).unwrap();
        assert_eq!(w.text("SELECT state FROM jobs"), "cancelled", "{how}");
        assert!(queued(&w, |j| matches!(j, Job::CommitHold { .. })) >= 1);
    }
    // A paid page with its document (its job scored) comes back live.
    let w = World::new();
    let (slug, _) = secret_page(&w, Some(principal(7)));
    let idx = hex::encode(w.peppers.current().id_h(&slug));
    w.h.exec(&format!("UPDATE detections SET kind = 'private_paid', owner_via = 'paid' WHERE id_h = x'{idx}'"));
    let (case_id, _, _) = w.send(CaseKind::Illegal, &slug, &fields, None);
    assert_eq!(state(&w, &slug, true), "disabled");
    w.op("reject", json!({"case_id": case_id, "reason": "not illegal"})).unwrap();
    assert_eq!(state(&w, &slug, true), "live");
}

/// The URL pipeline's ops on the route: `x_purge` (its dry run counts and
/// writes only its journal row; otherwise `DETECT_X = purged` and the first
/// purge page under the op's own journal seq) and `release_claim`.
#[test]
fn x_purge_and_release_claim_run_on_the_op_route() {
    let w = World::new();
    x_page(&w, "x5n9r3v", 1_890_000_000_000_000_000);
    let r = w.op("x_purge", json!({"reason": "check", "dry_run": true})).unwrap();
    assert_eq!((r["dry_run"].clone(), r["zero"].clone()), (json!(true), json!(false)), "{r}");
    assert_eq!(r["counts"]["runs"], 1, "{r}");
    assert_eq!(w.h.count("SELECT count(*) FROM switches WHERE name = 'DETECT_X'"), 0, "a dry run writes nothing");
    assert!(w.h.queue.borrow().is_empty());
    let r = w.op("x_purge", json!({"reason": "agreement ended", "dry_run": false})).unwrap();
    assert_eq!(r["dry_run"], false, "{r}");
    assert_eq!(w.text("SELECT state FROM switches WHERE name = 'DETECT_X'"), "purged");
    let seq = r["seq"].as_u64().unwrap();
    assert_eq!(w.h.count(&format!("SELECT count(*) FROM switches WHERE name = 'DETECT_X' AND journal_seq = {seq}")), 1);
    assert_eq!(queued(&w, |j| matches!(j, Job::XPurgePage { journal_seq } if *journal_seq == seq)), 1);
    assert_eq!(w.op("x_purge", json!({"reason": "r", "dry_run": "yes"})).err(), Some(RefusalCode::OpMalformed));
    // release_claim: the claim of a public page's URL check goes.
    w.h.db
        .execute(
            "INSERT INTO claims (claim_key, det_id_h, lease_until, stage) VALUES (?1, ?2, ?3, 't')",
            rusqlite::params![vec![0x61u8; 32], keys::public_id_h("k7m2xq9").to_vec(), w.h.now_ms() as i64 + 60_000],
        )
        .unwrap();
    let r = w.op("release_claim", json!({"reason": "stuck check", "slug": "k7m2xq9"})).unwrap();
    assert_eq!(r["released"], 1, "{r}");
    assert_eq!(w.h.count("SELECT count(*) FROM claims"), 0);
    assert_eq!(
        w.op("release_claim", json!({"reason": "r", "slug": "not a slug"})).err(),
        Some(RefusalCode::OpMalformed)
    );
}

#[test]
fn a_counter_notice_needs_the_manage_token_or_the_owner_and_never_keeps_it() {
    let w = World::new();
    let (slug, manage) = secret_page(&w, Some(principal(6)));
    let (case_id, _, _) = w.send(CaseKind::Copyright, &slug, &copyright("page"), None);
    w.op("remove", json!({"case_id": case_id, "reason": "valid notice"})).unwrap();
    // The removal left a strike on the page's owner.
    assert_eq!(w.h.count("SELECT count(*) FROM strikes WHERE owner_via = 'created'"), 1);
    let counter = [
        ("name", "Page Maker"),
        ("email", "maker@example.invalid"),
        ("address", "2 Fixture Road"),
        ("material", "my page"),
        ("mistake", "yes"),
        ("consent", "yes"),
        ("signature", "Page Maker"),
    ];
    let RemovalAnswer::Review(r) =
        w.removal(RemovalStep::Review, CaseKind::CounterNotice, &slug, &counter, None).unwrap()
    else {
        panic!()
    };
    assert_eq!(r.missing, vec!["manage"]);
    let mut wrong = counter.to_vec();
    wrong.push(("manage", "not-the-token"));
    let RemovalAnswer::Review(r) =
        w.removal(RemovalStep::Review, CaseKind::CounterNotice, &slug, &wrong, None).unwrap()
    else {
        panic!()
    };
    assert_eq!(r.missing, vec!["manage"]);
    // The owning account may counter-notify without the token.
    let RemovalAnswer::Review(r) =
        w.removal(RemovalStep::Review, CaseKind::CounterNotice, &slug, &counter, Some(principal(6))).unwrap()
    else {
        panic!()
    };
    assert!(r.complete);
    // The token proves the counter-notice; pasted again in a manage link
    // (in any case), it is masked there too.
    let pasted = format!("my page, https://s.site.example/{slug}/manage#m={}", manage.to_ascii_uppercase());
    let mut good: Vec<(&str, &str)> = counter.iter().copied().filter(|(k, _)| *k != "material").collect();
    good.push(("material", pasted.as_str()));
    good.push(("manage", manage.as_str()));
    let (cn, _, _) = w.send(CaseKind::CounterNotice, &slug, &good, None);
    let due: i64 = w.h.count(&format!("SELECT due_at FROM cases WHERE case_id = '{cn}'"));
    assert_eq!(due as u64, w.h.now_ms() + cases::COUNTER_WAIT_MS);
    assert_eq!(w.text(&format!("SELECT state FROM cases WHERE case_id = '{case_id}'")), "countered");
    let plain = keys::open_with(
        w.store.cases_key(),
        slop_ninja_detect_worker::queue::CASE_AAD,
        &w.h.doc(&format!("c/{cn}")).unwrap(),
    )
    .unwrap();
    let plain = String::from_utf8(plain).unwrap();
    assert!(!plain.to_ascii_lowercase().contains(&manage), "the token is never kept: {plain}");
    assert!(plain.contains(cases::TOKEN_MASK) && plain.contains(&slug), "{plain}");
    // The wait passes: status names the countered case to restore, with the
    // counter-notice beside it.
    let st = w.op("status", json!({})).unwrap();
    assert!(st["cases"]["restores_due"].as_array().unwrap().is_empty());
    w.h.advance(cases::COUNTER_WAIT_MS);
    let st = w.op("status", json!({})).unwrap();
    let due_at = w.h.count(&format!("SELECT due_at FROM cases WHERE case_id = '{cn}'"));
    assert_eq!(st["cases"]["restores_due"], json!([{"case_id": case_id, "counter_notice": cn, "due_at": due_at}]));
    // Restoring exactly the case status names brings the page back, and the
    // counter-notice moves on with it: nothing is left due or open.
    let named = st["cases"]["restores_due"][0]["case_id"].as_str().unwrap().to_owned();
    let r = w.op("restore", json!({"case_id": named, "reason": "counter-notice wait passed"})).unwrap();
    assert_eq!(r["pages"], 1, "{r}");
    assert_eq!(state(&w, &slug, true), "live");
    assert_eq!(w.page(&slug).unwrap().state.as_str(), "live");
    assert!(w.h.doc(&cases::quarantine_key(&case_id, &w.peppers.current().id_h(&slug))).is_none());
    for c in [&case_id, &cn] {
        assert_eq!(w.text(&format!("SELECT state FROM cases WHERE case_id = '{c}'")), "restored", "{c}");
    }
    let st = w.op("status", json!({})).unwrap();
    assert_eq!(st["cases"]["restores_due"], json!([]));
    assert_eq!(st["cases"]["open"], json!([]));
    // Nothing is countered behind the counter-notice any more.
    assert_eq!(w.op("restore", json!({"case_id": cn, "reason": "again"})).err(), Some(RefusalCode::NotFound));
}

/// Follows `status` exactly: restores each case `restores_due` names, in
/// order, until none is left; at most `bound` restores.
fn restore_what_status_names(w: &World, bound: usize) -> Vec<String> {
    let mut restored = Vec::new();
    for _ in 0..bound {
        let st = w.op("status", json!({})).unwrap();
        let due = st["cases"]["restores_due"].as_array().unwrap().clone();
        let Some(first) = due.first() else { return restored };
        let id = first["case_id"].as_str().unwrap().to_owned();
        w.op("restore", json!({"case_id": id, "reason": "counter-notice wait passed"})).unwrap();
        restored.push(id);
    }
    panic!("status still names a restore after {bound}: {restored:?}");
}

#[test]
fn a_due_counter_notice_restores_through_either_case_id() {
    let owner = principal(7);
    let counter = [
        ("name", "Page Maker"),
        ("email", "maker@example.invalid"),
        ("address", "2 Fixture Road"),
        ("material", "my page"),
        ("mistake", "yes"),
        ("consent", "yes"),
        ("signature", "Page Maker"),
    ];
    // Two copyright cases accepted against one page (the second finds it
    // removed already, so the first's quarantine holds it): status names
    // each in turn, and following it brings the page back.
    let w = World::new();
    let (slug, _) = secret_page(&w, Some(owner));
    let (x1, _, _) = w.send(CaseKind::Copyright, &slug, &copyright("page"), None);
    w.op("remove", json!({"case_id": x1, "reason": "valid notice"})).unwrap();
    let (x2, _, _) = w.send(CaseKind::Copyright, &slug, &copyright("page"), None);
    assert_eq!(w.op("remove", json!({"case_id": x2, "reason": "valid notice"})).unwrap()["pages"], 0);
    let (cn, _, _) = w.send(CaseKind::CounterNotice, &slug, &counter, Some(owner));
    for c in [&x1, &x2] {
        assert_eq!(w.text(&format!("SELECT state FROM cases WHERE case_id = '{c}'")), "countered");
    }
    w.h.advance(cases::COUNTER_WAIT_MS);
    let st = w.op("status", json!({})).unwrap();
    let named: Vec<(&str, &str)> = st["cases"]["restores_due"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| (e["case_id"].as_str().unwrap(), e["counter_notice"].as_str().unwrap()))
        .collect();
    let mut want = vec![(x1.as_str(), cn.as_str()), (x2.as_str(), cn.as_str())];
    want.sort();
    assert_eq!(named, want);
    let restored = restore_what_status_names(&w, 4);
    assert_eq!(restored.len(), 2, "{restored:?}");
    assert_eq!(state(&w, &slug, true), "live");
    for c in [&x1, &x2, &cn] {
        assert_eq!(w.text(&format!("SELECT state FROM cases WHERE case_id = '{c}'")), "restored", "{c}");
    }
    assert_eq!(w.op("status", json!({})).unwrap()["cases"]["open"], json!([]));

    // The counter-notice's own id restores the countered case behind it.
    let w = World::new();
    let (slug, _) = secret_page(&w, Some(owner));
    let (x, _, _) = w.send(CaseKind::Copyright, &slug, &copyright("page"), None);
    w.op("remove", json!({"case_id": x, "reason": "valid notice"})).unwrap();
    let (cn, _, _) = w.send(CaseKind::CounterNotice, &slug, &counter, Some(owner));
    w.h.advance(cases::COUNTER_WAIT_MS);
    let r = w.op("restore", json!({"case_id": cn, "reason": "counter-notice wait passed"})).unwrap();
    assert_eq!((r["pages"].clone(), r["restored_cases"].clone()), (json!(1), json!([x])), "{r}");
    assert_eq!(state(&w, &slug, true), "live");
    assert!(w.h.doc(&cases::quarantine_key(&x, &w.peppers.current().id_h(&slug))).is_none());
    for c in [&x, &cn] {
        assert_eq!(w.text(&format!("SELECT state FROM cases WHERE case_id = '{c}'")), "restored", "{c}");
    }
    let st = w.op("status", json!({})).unwrap();
    assert_eq!((st["cases"]["restores_due"].clone(), st["cases"]["open"].clone()), (json!([]), json!([])));

    // A counter-notice with no countered case behind it restores nothing
    // and changes nothing: the operator decides through the case that
    // holds the page.
    let (other, _) = secret_page(&w, Some(owner));
    let (lone, _, _) = w.send(CaseKind::CounterNotice, &other, &counter, Some(owner));
    let n = w.h.count("SELECT count(*) FROM op_journal");
    let before = w.h.dump();
    assert_eq!(w.op("restore", json!({"case_id": lone, "reason": "r"})).err(), Some(RefusalCode::NotFound));
    assert_eq!(w.h.count("SELECT count(*) FROM op_journal"), n);
    assert_eq!(w.h.dump(), before);
    assert_eq!(w.text(&format!("SELECT state FROM cases WHERE case_id = '{lone}'")), "received");
}

#[test]
fn strikes_count_created_pages_and_claimed_ones_after_review() {
    let w = World::new();
    let (a, _) = secret_page(&w, Some(principal(1)));
    let (b, manage_b) = secret_page(&w, None);
    // The second page is claimed by account 2 with its manage token.
    let id_b = w.peppers.current().id_h(&b);
    w.h.db
        .execute(
            "UPDATE detections SET owner_h = ?1, owner_via = 'claimed' WHERE id_h = ?2",
            rusqlite::params![w.store.owner_h(&[0xc2; 16]).to_vec(), id_b.to_vec()],
        )
        .unwrap();
    let _ = manage_b;
    let (ca, _, _) = w.send(CaseKind::Copyright, &a, &copyright("page"), None);
    let (cb, _, _) = w.send(CaseKind::Copyright, &b, &copyright("page"), None);
    w.op("remove", json!({"case_id": ca, "reason": "r"})).unwrap();
    w.op("remove", json!({"case_id": cb, "reason": "r"})).unwrap();
    let st = w.op("status", json!({})).unwrap();
    let strikes = st["cases"]["strikes"].as_array().unwrap().clone();
    let of =
        |n: u8| strikes.iter().find(|s| s["owner"] == json!(hex::encode(w.store.owner_h(&[n; 16])))).cloned().unwrap();
    assert_eq!((of(1)["counted"].clone(), of(1)["awaiting"].clone()), (json!(1), json!(0)));
    assert_eq!((of(0xc2)["counted"].clone(), of(0xc2)["awaiting"].clone()), (json!(0), json!(1)));
    assert_eq!(
        w.op("review_strike", json!({"case_id": cb, "counts": true, "reason": "uploaded it"})).unwrap()["reviewed"],
        1
    );
    let st = w.op("status", json!({})).unwrap();
    let strikes = st["cases"]["strikes"].as_array().unwrap();
    assert!(
        strikes.iter().any(|s| s["owner"] == json!(hex::encode(w.store.owner_h(&[0xc2; 16]))) && s["counted"] == 1)
    );
    // owner_match names the accounts behind a case's strikes, to the
    // operator only: the journal keeps no account id.
    {
        let mut u = w.accounts.users.borrow_mut();
        for n in [0u8, 1, 3, 0xc2] {
            u.push(UserEntry {
                user_id: UserId([n; 16]),
                state: if n == 0xc2 { UserState::Locked } else { UserState::Active },
            });
        }
    }
    let m = w.op("owner_match", json!({"case_id": cb, "reason": "repeat notices"})).unwrap();
    assert_eq!(
        m["matches"],
        json!([{"user_id": hex::encode([0xc2u8; 16]), "state": "locked", "owner_via": "claimed", "review": "counts"}])
    );
    assert_eq!(m["next"], Value::Null);
    let journal = w.h.dump();
    assert!(!journal.contains(&hex::encode([0xc2u8; 16])), "no account id in the store");
    // Many accounts: pages up to the bound, then `next`.
    {
        let mut u = w.accounts.users.borrow_mut();
        u.clear();
        for i in 0..(cases::OWNER_MATCH_PAGES as usize * 500 + 10) {
            let mut b = [0xffu8; 16];
            b[..8].copy_from_slice(&(i as u64).to_be_bytes());
            u.push(UserEntry { user_id: UserId(b), state: UserState::Active });
        }
    }
    w.accounts.calls.set(0);
    let m = w.op("owner_match", json!({"case_id": ca, "reason": "r"})).unwrap();
    assert_eq!(w.accounts.calls.get(), cases::OWNER_MATCH_PAGES);
    let next = m["next"].as_str().unwrap().to_owned();
    let m = w.op("owner_match", json!({"case_id": ca, "after": next, "reason": "r"})).unwrap();
    assert_eq!((m["scanned"].clone(), m["next"].clone()), (json!(10), Value::Null));
}

#[test]
fn notes_are_kept_encrypted_and_shown_to_the_operator_only() {
    let w = World::new();
    web_page(&w, "k7m2xq9", None);
    let (case_id, _, _) = w.send(
        CaseKind::OtherLegal,
        "k7m2xq9",
        &[("basis", "contract"), ("name", "N"), ("email", "e@e.invalid"), ("details", "please")],
        None,
    );
    w.op("case_note", json!({"case_id": case_id, "note": "called the claimant back", "reason": "log"})).unwrap();
    assert_eq!(w.text(&format!("SELECT state FROM cases WHERE case_id = '{case_id}'")), "reviewing");
    assert!(!w.h.dump().contains("called the claimant"), "no note in D1");
    let show = w.op("case_show", json!({"case_id": case_id, "reason": "review"})).unwrap();
    assert_eq!(show["notice"]["fields"]["basis"], "contract");
    assert_eq!(show["notes"][0]["note"], "called the claimant back");
    assert_eq!(show["targets"][0]["public_slug"], "k7m2xq9");
    // The note's journal row is redacted; every other row is the signed op.
    let rows: Vec<(String, Option<Vec<u8>>)> =
        w.h.db
            .prepare("SELECT op, signature FROM op_journal ORDER BY seq")
            .unwrap()
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap()
            .map(Result::unwrap)
            .collect();
    assert_eq!(rows.len(), 2);
    assert!(rows[0].0.contains("\"redacted\":true") && rows[0].1.is_none() && !rows[0].0.contains("claimant"));
    assert!(rows[1].1.is_some());
    assert_eq!(
        w.op("case_note", json!({"case_id": case_id, "note": "", "reason": "r"})).err(),
        Some(RefusalCode::OpMalformed)
    );
    assert_eq!(w.op("case_show", json!({"case_id": "td-nope", "reason": "r"})).err(), Some(RefusalCode::OpMalformed));
    assert_eq!(w.op("case_show", json!({"case_id": "td-00000000", "reason": "r"})).err(), Some(RefusalCode::NotFound));
}

// ----------------------------------------------------------------- X posts

/// Every journal row that holds `canon_key` verifies under the operator's
/// key when it is signed (the row is the signed op exactly).
fn verify_row(w: &World, op: &str, sig: &[u8]) {
    let v: Value = serde_json::from_str(op).unwrap();
    let wire = sop::Wire {
        v: 1,
        target: v["target"].as_str().unwrap().into(),
        env: v["env"].as_str().unwrap().into(),
        issued_ms: v["issued_ms"].as_u64().unwrap(),
        expires_ms: v["expires_ms"].as_u64().unwrap(),
        nonce: v["nonce"].as_str().unwrap().into(),
        op: v["op"].as_str().unwrap().into(),
        args: v["args"].clone(),
        key: sop::OperatorKey::from_verifying_key(w.key.verifying_key()).fingerprint(),
        sig: hex::encode(sig),
    };
    let verifier = sop::Verifier::new(
        sop::OperatorKey::from_verifying_key(w.key.verifying_key()),
        sop::Env::Staging,
        &[sop::Target::Detect],
    );
    verifier.verify_wire(&wire, wire.issued_ms + 1).unwrap();
}

#[test]
fn the_author_s_report_removes_an_x_page_at_once_and_leaves_only_the_hash() {
    let w = World::new();
    let run = x_page(&w, "x4m8q2t", 1855);
    let run2 = x_page(&w, "x5n9r3v", 1855);
    let fields = [
        ("relation", "author"),
        ("name", "Post Author"),
        ("email", "a@e.invalid"),
        ("verification", "a post from the account"),
    ];
    let (case_id, _, auto) = w.send(CaseKind::Author, "x4m8q2t", &fields, None);
    assert_eq!(auto, Some(AutoStep::Removed));
    for (slug, r) in [("x4m8q2t", run), ("x5n9r3v", run2)] {
        assert_eq!(state(&w, slug, false), "removed");
        assert!(w.h.doc(&format!("r/{r}")).is_none(), "the run document goes first");
        let p = w.page(slug).unwrap();
        assert_eq!((p.state.as_str(), p.ended.unwrap().code), ("removed", RefusalCode::XRemoved));
    }
    assert_eq!(w.h.count("SELECT count(*) FROM runs"), 0);
    // The tombstone keeps only the hash, and the case names it.
    let key = cases::x_canon_key(1855);
    let (url, st, rep, sid): (Option<String>, String, String, i64) =
        w.h.db
            .query_row(
                "SELECT canon_url, state, reputation, source_id FROM sources WHERE canon_key = ?1",
                [key.to_vec()],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .unwrap();
    assert_eq!((url, st.as_str(), rep.as_str()), (None, "opted_out", "unranked"));
    assert_eq!(w.h.count(&format!("SELECT source_id FROM cases WHERE case_id = '{case_id}'")), sid);
    assert_eq!(w.text(&format!("SELECT state FROM cases WHERE case_id = '{case_id}'")), "accepted");
    // One auto:x_tombstone row journals the removal by its hash.
    let (actor, op): (String, String) =
        w.h.db.query_row("SELECT actor, op FROM op_journal", [], |r| Ok((r.get(0)?, r.get(1)?))).unwrap();
    assert_eq!(actor, "auto:x_tombstone");
    assert_eq!(
        op,
        format!("{{\"args\":{{\"canon_key\":\"{}\",\"reason\":\"author\"}},\"op\":\"x_remove\"}}", hex::encode(key))
    );
    // Nothing but the source row's key names the post: no /status/, no id.
    let dump = w.h.dump();
    assert!(!dump.contains("/status/") && !dump.contains("1855"), "{dump}");
    // The legal notice goes as for any case; the purges cover both pages.
    assert_eq!(queued(&w, |j| matches!(j, Job::LegalNotice { .. })), 1);
    assert!(queued(&w, |j| matches!(j, Job::Purge { urls } if urls.iter().any(|u| u.ends_with("/d/x5n9r3v/x")))) >= 1);
    // Restore lifts the opt-out and nothing more.
    let r = w.op("restore", json!({"case_id": case_id, "reason": "author changed their mind"})).unwrap();
    assert_eq!(r["ok"], true);
    assert_eq!(w.h.count("SELECT count(*) FROM sources"), 0);
    assert_eq!(w.h.count("SELECT count(*) FROM detections WHERE state = 'removed' AND source_id IS NULL"), 2);
    assert_eq!(w.text(&format!("SELECT state FROM cases WHERE case_id = '{case_id}'")), "restored");
    // A second restore of the same case changes nothing.
    assert_eq!(w.op("restore", json!({"case_id": case_id, "reason": "again"})).unwrap()["pages"], 0);
}

#[test]
fn x_remove_signs_only_the_hash_and_works_for_a_post_never_held() {
    let w = World::new();
    x_page(&w, "x4m8q2t", 2024);
    let key = cases::x_canon_key(2024);
    let a = w.op("x_remove", json!({"reason": "x", "canon_key": hex::encode(key)})).unwrap();
    assert_eq!(a["removed"], 1);
    let case_id = a["case_id"].as_str().unwrap().to_owned();
    assert_eq!(w.text(&format!("SELECT kind FROM cases WHERE case_id = '{case_id}'")), "x_request");
    assert_eq!(
        w.h.count(&format!(
            "SELECT count(*) FROM cases WHERE case_id = '{case_id}' AND notice_key IS NULL AND status_h IS NULL"
        )),
        1
    );
    assert_eq!(queued(&w, |j| matches!(j, Job::LegalNotice { .. })), 0, "the request stays with the user's mail");
    // Its journal row is the signed op: it verifies and names only the hash.
    let (op, sig): (String, Vec<u8>) =
        w.h.db.query_row("SELECT op, signature FROM op_journal", [], |r| Ok((r.get(0)?, r.get(1)?))).unwrap();
    verify_row(&w, &op, &sig);
    assert!(op.contains(&hex::encode(key)) && !op.contains("2024") && !op.contains("status"));
    assert_eq!(w.page("x4m8q2t").unwrap().removal.unwrap().basis, "x");
    // A post never held: the tombstone is written, so no check can start.
    let other = cases::x_canon_key(77);
    let a = w.op("x_remove", json!({"reason": "author", "canon_key": hex::encode(other)})).unwrap();
    assert_eq!(a["removed"], 0);
    assert_eq!(
        w.h.count(&format!(
            "SELECT count(*) FROM sources WHERE hex(canon_key) = '{}' AND state = 'opted_out'",
            hex::encode_upper(other)
        )),
        1
    );
    // The key's exact form, and the reasons.
    for bad in [
        hex::encode_upper(key),
        hex::encode(key)[..62].to_owned(),
        "zz".repeat(32),
        "https://x.com/i/status/2024".into(),
    ] {
        assert_eq!(w.op("x_remove", json!({"reason": "x", "canon_key": bad})).err(), Some(RefusalCode::OpMalformed));
    }
    assert_eq!(
        w.op("x_remove", json!({"reason": "spam", "canon_key": hex::encode(key)})).err(),
        Some(RefusalCode::OpMalformed)
    );
    // `remove {slug}` on an X page is one removal too, with its own case.
    x_page(&w, "x6p2s4w", 3030);
    let r = w.op("remove", json!({"slug": "x6p2s4w", "reason": "unchecked for a day"})).unwrap();
    assert_eq!(r["pages"], 1);
    let xc = r["x_case_id"].as_str().unwrap();
    assert_eq!(w.text(&format!("SELECT kind FROM cases WHERE case_id = '{xc}'")), "x_request");
    assert_eq!(
        w.h.count("SELECT count(*) FROM op_journal WHERE actor = 'auto:x_tombstone' AND op LIKE '%\"other\"%'"),
        1
    );
}

// ------------------------------------------------------------ the envelope

#[test]
fn the_envelope_is_checked_before_anything_runs() {
    let w = World::new();
    let ok = w.signed(sop::Target::Detect, sop::Env::Staging, "status", json!({}));
    let body = serde_json::to_vec(&ok).unwrap();
    assert!(w.post(&body).is_ok());
    // The same op again: refused, and journaled once.
    assert_eq!(w.post(&body).err(), Some(RefusalCode::OpReusedNonce));
    assert_eq!(w.h.count("SELECT count(*) FROM op_journal"), 1);
    // Another env, another target, an account op, an edge op on a detect op.
    let prod = w.signed(sop::Target::Detect, sop::Env::Production, "status", json!({}));
    assert_eq!(w.post(&serde_json::to_vec(&prod).unwrap()).err(), Some(RefusalCode::OpWrongEnv));
    let acc = w.signed(sop::Target::Account, sop::Env::Staging, "disable_account", json!({"reason": "r"}));
    assert_eq!(w.post(&serde_json::to_vec(&acc).unwrap()).err(), Some(RefusalCode::OpWrongTarget));
    let edge = w.signed(sop::Target::Edge, sop::Env::Staging, "status", json!({}));
    assert_eq!(w.post(&serde_json::to_vec(&edge).unwrap()).err(), Some(RefusalCode::OpUnknown));
    // A link beside the envelope, or a wrapper: malformed.
    let mut v = serde_json::to_value(w.signed(
        sop::Target::Detect,
        sop::Env::Staging,
        "restore",
        json!({"case_id": "td-00000000", "reason": "r"}),
    ))
    .unwrap();
    let wrapped = json!({"signed": v.clone(), "x_url": "https://x.com/i/status/2"});
    v["x_url"] = "https://x.com/i/status/2".into();
    for b in [v, wrapped] {
        assert_eq!(w.post(&serde_json::to_vec(&b).unwrap()).err(), Some(RefusalCode::OpMalformed));
    }
    // A bad signature, an expired op.
    let mut forged = w.signed(sop::Target::Detect, sop::Env::Staging, "status", json!({}));
    forged.issued_ms += 1;
    assert_eq!(w.post(&serde_json::to_vec(&forged).unwrap()).err(), Some(RefusalCode::OpBadSignature));
    let late = w.signed(sop::Target::Detect, sop::Env::Staging, "status", json!({}));
    w.h.advance(60_000);
    assert_eq!(w.post(&serde_json::to_vec(&late).unwrap()).err(), Some(RefusalCode::OpExpired));
    // Ops of other builds spend no nonce; a missing reason is refused.
    let n = w.h.count("SELECT count(*) FROM op_journal");
    for name in ops::NOT_BUILT.iter().filter(|o| **o != "edge_coverage") {
        assert_eq!(
            w.op(name, json!({"reason": "r", "dry_run": true})).err(),
            Some(RefusalCode::NotConfigured),
            "{name}"
        );
    }
    assert_eq!(
        w.op("set_switch", json!({"name": "DETECT_PAGES", "state": "off"})).err(),
        Some(RefusalCode::OpMalformed)
    );
    assert_eq!(
        w.op("set_switch", json!({"name": "DETECT_PAGES", "state": "off", "reason": "r", "extra": 1})).err(),
        Some(RefusalCode::OpMalformed)
    );
    assert_eq!(w.h.count("SELECT count(*) FROM op_journal"), n);
    // An op too large, and no key configured.
    assert_eq!(w.post(&vec![b' '; ops::OP_BODY_MAX + 1]).err(), Some(RefusalCode::TooLarge));
    w.h.vars.borrow_mut().remove("DETECT_OP_KEY");
    assert_eq!(w.op("status", json!({})).err(), Some(RefusalCode::NotConfigured));
}

#[test]
fn store_ops_apply_with_their_journal_row() {
    let w = World::new();
    w.op("set_switch", json!({"name": "DETECT_PAGES", "state": "off", "reason": "legal emergency"})).unwrap();
    assert_eq!(w.text("SELECT state FROM switches WHERE name = 'DETECT_PAGES'"), "off");
    assert_eq!(
        w.h.count("SELECT journal_seq FROM switches WHERE name = 'DETECT_PAGES'"),
        w.h.count("SELECT max(seq) FROM op_journal")
    );
    w.op("set_switch", json!({"name": "DETECT_PAGES", "state": "on", "reason": "over"})).unwrap();
    let (slug, _) = secret_page(&w, None);
    let id = w.peppers.current().id_h(&slug);
    let r = w.op("delete_private", json!({"id_h": hex::encode(id), "reason": "court order"})).unwrap();
    assert_eq!(r["deleted"], 1);
    assert_eq!(state(&w, &slug, true), "deleted");
    assert!(w.h.doc(&format!("p/{}", hex::encode(id))).is_none());
    w.op("purge", json!({"urls": ["https://site.example/d/k7m2xq9"], "reason": "stale"})).unwrap();
    assert_eq!(
        w.op("purge", json!({"urls": ["https://elsewhere.example/"], "reason": "r"})).err(),
        Some(RefusalCode::OpMalformed)
    );
    let st = w.op("status", json!({})).unwrap();
    assert!(st["store"]["detections"].is_array() && st["cases"]["cases"].is_array());
    // Every row is the op exactly and verifies.
    let rows: Vec<(String, Vec<u8>)> =
        w.h.db
            .prepare("SELECT op, signature FROM op_journal ORDER BY seq")
            .unwrap()
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap()
            .map(Result::unwrap)
            .collect();
    assert_eq!(rows.len(), 5);
    for (op, sig) in rows {
        verify_row(&w, &op, &sig);
    }
    let _: BTreeMap<String, String> = BTreeMap::new();
}

#[test]
fn the_legal_stream_never_carries_a_secret_slug() {
    let w = World::new();
    let (slug, _) = secret_page(&w, Some(principal(8)));
    let link = format!("https://s.site.example/{slug}");
    // The claimant pastes the page's link again, in another case and with
    // its confusable letters, beside the target.
    let pasted = format!("See {} and {link}?x=1 for the whole text.", link.to_ascii_uppercase().replace('0', "O"));
    let fields = [("category", "other"), ("explanation", pasted.as_str()), ("good_faith", "yes")];
    let (case_id, _, auto) = w.send(CaseKind::Illegal, &link, &fields, None);
    assert_eq!(auto, Some(AutoStep::Disabled));
    let msg = w.h.queue.borrow().iter().find(|m| matches!(m.job, Job::LegalNotice { .. })).cloned().unwrap();
    assert_eq!(
        block_on(slop_ninja_detect_worker::queue::consume(&w.h, &w.store, &msg)),
        slop_ninja_detect_worker::queue::Ack::Done
    );
    // What the relay keeps and mails names the page only as a private
    // detection, wherever the slug was.
    let posted = w.h.relay.borrow()[0].clone();
    let text = String::from_utf8(posted.clone()).unwrap();
    let folded: String = text.bytes().map(|b| cases::fold_byte(b) as char).collect();
    assert!(!folded.contains(&slug), "the relay body holds the slug: {text}");
    let body: Value = serde_json::from_slice(&posted).unwrap();
    assert_eq!(body["targets"], json!([cases::PRIVATE_TARGET]));
    assert_eq!(body["notice"]["target"], cases::PRIVATE_TARGET);
    assert_eq!(body["notice"]["secret"], true);
    let explanation = body["notice"]["fields"]["explanation"].as_str().unwrap();
    assert_eq!(
        explanation,
        format!(
            "See HTTPS://S.SITE.EXAMPLE/{m} and https://s.site.example/{m}?x=1 for the whole text.",
            m = cases::SLUG_MASK
        )
    );
    // The sealed notice keeps the link for the operator's case_show.
    let shown = w.op("case_show", json!({"case_id": case_id, "reason": "review"})).unwrap();
    assert_eq!(shown["notice"]["target"], json!(link));
    assert!(shown["notice"]["fields"]["explanation"].as_str().unwrap().contains(&link));
    // A public page's notice reaches the relay as it was written.
    web_page(&w, "k7m2xq9", None);
    w.h.queue.borrow_mut().clear();
    w.send(CaseKind::Copyright, "k7m2xq9", &copyright("excerpt"), None);
    let msg = w.h.queue.borrow().iter().find(|m| matches!(m.job, Job::LegalNotice { .. })).cloned().unwrap();
    assert_eq!(
        block_on(slop_ninja_detect_worker::queue::consume(&w.h, &w.store, &msg)),
        slop_ninja_detect_worker::queue::Ack::Done
    );
    let body: Value = serde_json::from_slice(&w.h.relay.borrow()[1]).unwrap();
    assert_eq!(body["notice"]["target"], "https://site.example/d/k7m2xq9");
    assert_eq!(body["notice"]["secret"], false);
    assert_eq!(body["notice"]["fields"]["work"], "https://writer.example/essay");
}

#[test]
fn masking_folds_as_a_page_path_does() {
    let slug = "0123456789abcdefghjk";
    assert_eq!(cases::mask_folded("x 0123456789ABCDEFGHJK y", slug, "[m]"), "x [m] y");
    assert_eq!(cases::mask_folded("Ol23456789abcdefghjk", slug, "[m]"), "[m]");
    assert_eq!(cases::mask_folded("é0123456789abcdefghjké", slug, "[m]"), "é[m]é");
    assert_eq!(cases::mask_folded("0123456789abcdefghj", slug, "[m]"), "0123456789abcdefghj");
    assert_eq!(cases::mask_folded("", slug, "[m]"), "");
    // A notice whose secret target does not end in a slug is not sent.
    let n = cases::Notice {
        v: cases::NOTICE_V,
        track: CaseKind::Illegal,
        target: "https://s.site.example/".into(),
        secret: true,
        fields: BTreeMap::new(),
        received_at: 1,
        signed_in: false,
    };
    assert!(n.for_relay().is_none());
}

#[test]
fn a_purge_naming_a_secret_page_keeps_no_slug_in_the_journal() {
    let w = World::new();
    let (slug, _) = secret_page(&w, None);
    let secret_url = format!("https://s.site.example/{slug}");
    let site_url = "https://site.example/d/k7m2xq9";
    let n = w.h.count("SELECT count(*) FROM op_journal");
    w.op("purge", json!({"urls": [site_url, secret_url], "reason": "removed page"})).unwrap();
    // On the site origin, a path holding a slug-length run counts as secret too.
    let typed = format!("https://site.example/{}", slug.to_ascii_uppercase());
    w.op("purge", json!({"urls": [typed], "reason": "typed on the wrong host"})).unwrap();
    assert_eq!(w.h.count("SELECT count(*) FROM op_journal"), n + 2);
    // Both rows are redacted: unsigned, the nonce kept, no slug in any case.
    let rows: Vec<String> = w
        .h
        .db
        .prepare("SELECT op FROM op_journal WHERE seq > ?1 AND signature IS NULL AND nonce_h IS NOT NULL ORDER BY seq")
        .unwrap()
        .query_map([n], |r| r.get(0))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    assert_eq!(rows.len(), 2);
    for op in &rows {
        assert!(!op.to_ascii_lowercase().contains(&slug), "{op}");
    }
    let first: Value = serde_json::from_str(&rows[0]).unwrap();
    assert_eq!(first, json!({"op": "purge", "args": {"urls": [site_url], "secret_urls": 1}, "redacted": true}));
    assert!(!w.h.dump().to_ascii_lowercase().contains(&slug));
    // The purge itself still goes (DESIGN 7.13: from the request that carried the slug).
    assert_eq!(queued(&w, |j| matches!(j, Job::Purge { urls } if urls.contains(&secret_url))), 1);
    // A purge of site pages alone keeps its signed row.
    w.op("purge", json!({"urls": [site_url], "reason": "stale"})).unwrap();
    let (op, sig): (String, Vec<u8>) = w
        .h
        .db
        .query_row("SELECT op, signature FROM op_journal ORDER BY seq DESC LIMIT 1", [], |r| Ok((r.get(0)?, r.get(1)?)))
        .unwrap();
    verify_row(&w, &op, &sig);
    for (url, secret) in [
        ("https://site.example/d/k7m2xq9/card.png", false),
        ("https://site.example/detect/removals/", false),
        ("https://s.site.example/anything", true),
        ("https://elsewhere.example/x", true),
        ("https://site.example/abcdefghjkmnpqrstvwxyz", true),
    ] {
        assert_eq!(ops::names_secret(&w.origins, url), secret, "{url}");
    }
}
