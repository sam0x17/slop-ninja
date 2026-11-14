//! The store's flows on the detect store's own migration (in-memory SQLite,
//! foreign keys on): links, manage, owners, re-runs, copies, expiry, the
//! backstop, states, the queue and the page JSON of every state.

mod common;

use common::{Harness, block_on};
use slop_ninja_detect_page::api::{ManageRequest, SaveReason, ShareRequest};
use slop_ninja_detect_page::codes::{DetState, Expiry, ManageAction, OwnerAction, OwnerRelation, PurgeMode};
use slop_ninja_detect_page::ids::{IdH, UserId};
use slop_ninja_detect_page::internal::{OwnerRequest, StatesRequest};
use slop_ninja_detect_page::origins::Origins;
use slop_ninja_detect_page::page::{Page, Run};
use slop_ninja_detect_page::queue::{Job, QueueMessage};
use slop_ninja_detect_page::refusal::RefusalCode;
use slop_ninja_detect_page::sn::Principal;
use slop_ninja_detect_worker::canon;
use slop_ninja_detect_worker::doc::{SecretDoc, Serving, StoredResult};
use slop_ninja_detect_worker::keys::{Pepper, Peppers, StoreKeys};
use slop_ninja_detect_worker::manage::{OwnerAnswer, manage, owner, states};
use slop_ninja_detect_worker::queue::{Ack, consume};
use slop_ninja_detect_worker::reads;
use slop_ninja_detect_worker::rerun::{Scorer, rerun_secret};
use slop_ninja_detect_worker::share::{Keys, Sealed, share};
use slop_ninja_detect_worker::store::secret_doc_key;
use slop_ninja_detect_worker::{cron, keys};

const DAY: u64 = 86_400_000;
const DIGEST: &str = "e2536d12dff593d7b11895b3cbf6b10232601abfbebde248325afd707722b5e8";
const TEXT: &str = "Tomato notes\n\nWe planted the tomatoes late this year because the frost hung around until the \
second week of May. The rain barrel overflowed twice in June, so we moved it under the shed gutter. Next year we \
want a proper cold frame and a second compost bay.\r\n";

struct World {
    h: Harness,
    store: StoreKeys,
    peppers: Peppers,
    origins: Origins,
    serving: Serving,
}

impl World {
    fn new() -> World {
        World {
            h: Harness::new(),
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
        }
    }

    fn keys(&self) -> Keys<'_> {
        Keys { store: &self.store, peppers: &self.peppers }
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

/// A banded result with no passages (they would point past this text).
fn result(load: &str) -> StoredResult {
    let mut r = stored(fixture("15-secret-text").run.as_ref().unwrap(), load);
    r.spans.clear();
    r
}

fn sealed(w: &World, text: &str, load: &str) -> Sealed {
    let mut nonce = vec![0u8; 12];
    slop_ninja_detect_worker::db::Io::fill(&w.h, &mut nonce);
    Sealed { text_sha256: canon::text_sha256(text), nonce, result: result(load) }
}

fn req(text: &str, save: bool, source: Option<&str>) -> ShareRequest {
    ShareRequest {
        text: text.into(),
        seal: "opened-elsewhere".into(),
        expiry: Expiry::Keep,
        save,
        source: source.map(Into::into),
    }
}

fn principal(n: u8) -> Principal {
    Principal { user_id: UserId([n; 16]), auth_time_ms: 1, flags: 0 }
}

fn make_link(w: &World, load: &str, expiry: Expiry, who: Option<Principal>) -> (String, String) {
    let mut r = req(TEXT, who.is_some(), None);
    r.expiry = expiry;
    let a = block_on(share(&w.h, w.keys(), &r, Some(sealed(w, TEXT, load)), who)).unwrap();
    (a.slug, a.manage)
}

fn page(w: &World, slug: &str) -> Result<Page, RefusalCode> {
    block_on(reads::page(&w.h, w.keys(), &w.serving, slug))
}

fn id_of(w: &World, slug: &str) -> [u8; 32] {
    w.peppers.current().id_h(slug)
}

fn state_of(w: &World, slug: &str) -> String {
    w.h.db.query_row("SELECT state FROM detections WHERE id_h = ?1", [id_of(w, slug).to_vec()], |r| r.get(0)).unwrap()
}

// ------------------------------------------------------------------- links

#[test]
fn a_link_stores_the_text_encrypted_and_nothing_of_it_in_d1() {
    let w = World::new();
    let (slug, manage_t) = make_link(&w, "load-new", Expiry::Week, None);
    assert_eq!(slug.len(), 20);
    assert_eq!(manage_t.len(), 26);
    let id = id_of(&w, &slug);
    let doc = w.h.doc(&secret_doc_key(&id)).unwrap();
    // The document is ciphertext; it opens only under the page's own key.
    assert!(!String::from_utf8_lossy(&doc).contains("tomatoes"));
    let plain = w.peppers.current().doc_key(&slug).open(&doc).unwrap();
    let d = SecretDoc::decode(&plain).unwrap();
    assert_eq!(d.text, canon::canon_v1(TEXT), "the exact string that was scored: the canonical form");
    // No word of the text, no slug and no token anywhere in D1.
    let dump = w.h.dump();
    for word in ["tomatoes", "frost", "barrel", "compost"] {
        assert!(!dump.contains(word), "{word} in D1");
    }
    assert!(!dump.contains(&slug) && !dump.contains(&manage_t));
    let (kind, platform, state, exp, owner): (String, String, String, i64, Option<Vec<u8>>) =
        w.h.db
            .query_row(
                "SELECT kind, platform, state, expires_at, owner_h FROM detections WHERE id_h = ?1",
                [id.to_vec()],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
            )
            .unwrap();
    assert_eq!((kind.as_str(), platform.as_str(), state.as_str()), ("private_text", "paste", "live"));
    assert_eq!(exp as u64, w.h.now.get() + 7 * DAY);
    assert!(owner.is_none());
    let p = page(&w, &slug).unwrap();
    assert_eq!(p.state, DetState::Live);
    // The page shows the text the scorer read: its canonical form.
    assert_eq!(p.text.unwrap().body.as_deref(), Some(canon::canon_v1(TEXT).as_ref()));
    assert!(p.source.is_none());
    assert!(!p.serving.differs_from_run);
}

#[test]
fn a_seal_makes_one_link_and_only_for_its_own_text() {
    let w = World::new();
    let s = sealed(&w, TEXT, "load-new");
    block_on(share(&w.h, w.keys(), &req(TEXT, false, None), Some(s.clone()), None)).unwrap();
    assert_eq!(block_on(share(&w.h, w.keys(), &req(TEXT, false, None), Some(s), None)), Err(RefusalCode::SealUsed));
    // The same text with other line ends and whitespace shares its canonical
    // form; any other edit does not.
    let s2 = sealed(&w, TEXT, "load-new");
    let edited = TEXT.replace("late", "early");
    assert_eq!(
        block_on(share(&w.h, w.keys(), &req(&edited, false, None), Some(s2.clone()), None)),
        Err(RefusalCode::SealInvalid)
    );
    let respaced = format!("  {}", TEXT.replace("\r\n", "\n"));
    let a = block_on(share(&w.h, w.keys(), &req(&respaced, false, None), Some(s2), None)).unwrap();
    // Its page shows the canonical form, as the first link's does
    // (`an_equal_canonical_text_keeps_its_passages_on_the_same_words`).
    assert_eq!(page(&w, &a.slug).unwrap().text.unwrap().body.as_deref(), Some(canon::canon_v1(TEXT).as_ref()));
    assert_eq!(block_on(share(&w.h, w.keys(), &req(TEXT, false, None), None, None)), Err(RefusalCode::SealInvalid));
    assert_eq!(w.h.count("SELECT count(*) FROM detections"), 2);
    assert_eq!(w.h.docs.borrow().len(), 2, "a refused share leaves no document");
}

/// A seal's passages are offsets into the canonical text. A link or a
/// preview asked for with another string of the same canonical form
/// (surrounding whitespace, CRLF or a lone CR, NFD accents before and inside
/// the passage) stores and renders the canonical text, with each passage on
/// the words the result named, never shifted onto others.
#[test]
fn an_equal_canonical_text_keeps_its_passages_on_the_same_words() {
    use slop_ninja_detect_worker::answer::{PackageLine, ResultFull, Span};
    use slop_ninja_detect_worker::{handoff, seal};
    let w = World::new();
    let secret: [u8; 32] = *b"store secret bytes, for tests ok";
    let now = 1_795_169_040_000;
    let para = "The frost hung around until the second week of May, and the rain barrel overflowed twice in June.";
    let passage = format!(
        "We moved the caf\u{e9} table under the shed gutter. {para} The r\u{e9}sum\u{e9} of the season is short."
    );
    let canonical = format!(
        "Caf\u{e9} notes \u{1F345}\n\n{para}\n\n{para} {para}\n\n{para}\n\n{passage}\n\n{para}\n\n{para} {para}"
    );
    assert_eq!(canon::canon_v1(&canonical), canonical.as_str());
    let c16 = |s: &str| s.encode_utf16().count();
    let start = c16(&canonical[..canonical.find(&passage).unwrap()]);
    let end = start + c16(&passage);
    let (total, words) = (canonical.split_whitespace().count(), passage.split_whitespace().count() as u64);
    assert!(total >= 150 && words >= 24);
    // The line the score route seals: offsets into the canonical text.
    let full = ResultFull {
        band: "very_strong".into(),
        confidence_tenths: Some(999),
        confidence_text: Some("above 99.9%".into()),
        share_text: Some("about 30%".into()),
        share_pct: Some(30),
        words: total,
        total_words: total,
        cut: "none".into(),
        scored_end: c16(&canonical),
        tokens: 260,
        length: "150_450".into(),
        spans: Some(vec![Span { start, end, words, confidence: Some(98) }]),
        spans_note: None,
        markers: vec![],
        invisible: false,
        package: PackageLine { digest: DIGEST.into(), publication_pending: true },
    };
    let payload = seal::ResultPayload {
        day: seal::day_of(now),
        text_sha256: seal::text_sha256(&canonical),
        scored_end: full.scored_end,
        cut: "none".into(),
        package_load: "load-new".into(),
        policy_v: seal::POLICY_V.into(),
        result_full: full,
        issued_ms: now,
    };
    let sealed = seal::seal(&secret, seal::Purpose::Result, seal::day_of(now), [9; 12], &payload).unwrap();
    let open = |text: &str| handoff::sealed(seal::open_result(&secret, now + 1, &sealed, text).unwrap(), &w.serving);
    // Shifted strings with the same canonical form: every offset after the
    // leading whitespace, each CRLF and each decomposed accent moves.
    let nfd = canonical.replace('\u{e9}', "e\u{301}");
    let shifted = format!("\u{3000} \t{}\r\n \r\n", nfd.replace('\n', "\r\n"));
    let lone_cr = format!("\n\n  {}  ", nfd.replace('\n', "\r"));
    for s in [&shifted, &lone_cr] {
        assert_eq!(canon::canon_v1(s), canonical.as_str());
        assert_ne!(s.as_str(), canonical.as_str());
    }
    let on_the_words = |p: &Page, what: &str| {
        let body = p.text.as_ref().unwrap().body.as_deref().unwrap();
        assert_eq!(body, canonical, "{what}: the canonical text");
        let b16: Vec<u16> = body.encode_utf16().collect();
        let spans = &p.run.as_ref().unwrap().spans;
        assert_eq!(spans.len(), 1, "{what}");
        let drawn = String::from_utf16(&b16[spans[0].start as usize..spans[0].end as usize]).unwrap();
        assert_eq!(canon::canon_v1(&drawn), passage.as_str(), "{what}: the passage's own words");
    };
    // The preview of either string, and a link made from the shifted one.
    for s in [&shifted, &lone_cr] {
        let p = block_on(reads::preview(&w.h, &w.serving, s, Some(&open(s).unwrap()))).unwrap();
        on_the_words(&p, "preview");
    }
    let a = block_on(share(&w.h, w.keys(), &req(&shifted, false, None), Some(open(&shifted).unwrap()), None)).unwrap();
    on_the_words(&page(&w, &a.slug).unwrap(), "link");
}

#[test]
fn the_store_switch_stops_new_links() {
    let w = World::new();
    w.h.exec("INSERT INTO switches (name, state, at, journal_seq) VALUES ('DETECT_STORE', 'off', 1, 1)");
    let r = block_on(share(&w.h, w.keys(), &req(TEXT, false, None), Some(sealed(&w, TEXT, "l")), None));
    assert_eq!(r, Err(RefusalCode::StoreOff));
    w.h.exec("UPDATE switches SET state = 'on'");
    w.h.vars.borrow_mut().insert("DETECT_STORE".into(), "off".into());
    let r = block_on(share(&w.h, w.keys(), &req(TEXT, false, None), Some(sealed(&w, TEXT, "l")), None));
    assert_eq!(r, Err(RefusalCode::StoreOff), "the var is the hard off");
    assert!(w.h.docs.borrow().is_empty());
}

#[test]
fn a_pasted_x_source_is_ordinary_paste() {
    let w = World::new();
    let r = req(TEXT, false, Some("https://x.com/a/status/1"));
    let a = block_on(share(&w.h, w.keys(), &r, Some(sealed(&w, TEXT, "load-new")), None)).unwrap();
    let p = page(&w, &a.slug).unwrap();
    assert_eq!(p.platform.as_str(), "paste");
    let body = p.text.as_ref().unwrap().body.as_deref();
    assert_eq!(body, Some(canon::canon_v1(TEXT).as_ref()), "the full text is stored and shown");
    // The plain host line, exactly as the contract's pasted-X fixture.
    assert_eq!(p.source, fixture("19-secret-pasted-x-source").source);
    assert!(!w.h.dump().contains("x.com"), "the source never reaches D1");
    // A source that is not a host is refused before anything is stored.
    // Userinfo before the host (built at run time: no address-like literal).
    let userinfo = format!("https://user{}x.com/", '@');
    let bad = req(TEXT, false, Some(&userinfo));
    let r = block_on(share(&w.h, w.keys(), &bad, Some(sealed(&w, TEXT, "load-new")), None));
    assert_eq!(r, Err(RefusalCode::BadUrl));
}

#[test]
fn a_save_owns_the_link_and_a_failed_save_releases_it() {
    let w = World::new();
    let who = principal(1);
    let (slug, _) = make_link(&w, "load-new", Expiry::Keep, Some(who));
    let added = w.h.owned.borrow()[0].clone();
    assert_eq!(added.slug, slug);
    assert!(added.is_owner);
    assert_eq!(added.id_h, IdH(id_of(&w, &slug)));
    let owner_h: Vec<u8> =
        w.h.db
            .query_row("SELECT owner_h FROM detections WHERE id_h = ?1", [id_of(&w, &slug).to_vec()], |r| r.get(0))
            .unwrap();
    assert_eq!(owner_h, w.store.owner_h(&[1u8; 16]).to_vec());
    // The account side fails: the link stays, unowned.
    w.h.owned_fails.set(true);
    let a = block_on(share(&w.h, w.keys(), &req(TEXT, true, None), Some(sealed(&w, TEXT, "l")), Some(who))).unwrap();
    assert!(!a.saved);
    assert_eq!(a.reason, Some(SaveReason::AccountUnavailable));
    assert_eq!(w.h.count("SELECT count(*) FROM detections WHERE owner_h IS NULL AND owner_via IS NULL"), 1);
    // Save without a session.
    let a = block_on(share(&w.h, w.keys(), &req(TEXT, true, None), Some(sealed(&w, TEXT, "l")), None)).unwrap();
    assert_eq!((a.saved, a.reason), (false, Some(SaveReason::SignedOut)));
}

// ------------------------------------------------------------------ manage

fn mreq(slug: &str, t: &str, action: ManageAction, expiry: Option<Expiry>) -> ManageRequest {
    ManageRequest { slug: slug.into(), manage: t.into(), action, expiry }
}

#[test]
fn delete_by_token_tombstones_the_row_and_its_document() {
    let w = World::new();
    let (slug, t) = make_link(&w, "load-new", Expiry::Keep, Some(principal(1)));
    let wrong = block_on(manage(&w.h, w.keys(), &w.origins, &mreq(&slug, &"0".repeat(26), ManageAction::Delete, None)));
    assert_eq!(wrong, Err(RefusalCode::ManageInvalid));
    let a = block_on(manage(&w.h, w.keys(), &w.origins, &mreq(&slug, &t, ManageAction::Delete, None))).unwrap();
    assert_eq!(a.state, DetState::Deleted);
    let id = id_of(&w, &slug);
    assert!(w.h.doc(&secret_doc_key(&id)).is_none());
    type Tomb = (String, Option<Vec<u8>>, Option<Vec<u8>>, Option<String>, i64);
    let tomb: Tomb =
        w.h.db
            .query_row(
                "SELECT state, manage_h, owner_h, doc_key, state_v FROM detections WHERE id_h = ?1",
                [id.to_vec()],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
            )
            .unwrap();
    assert_eq!(tomb, ("deleted".into(), None, None, None, 2));
    // The purge names the secret page and its card on the secret origin.
    let q = w.h.queue.borrow();
    let Job::Purge { urls } = &q[0].job else { panic!() };
    assert_eq!(
        urls,
        &vec![format!("https://s.site.example/{slug}"), format!("https://s.site.example/{slug}/card.png")]
    );
    drop(q);
    let p = page(&w, &slug).unwrap();
    assert_eq!((p.state, p.http_status()), (DetState::Deleted, 410));
    assert_eq!(p, {
        let mut f = fixture("21-secret-deleted");
        f.slug = Some(slug.clone());
        f.created_at = p.created_at;
        f
    });
    // A second delete answers the page's state.
    let again = block_on(manage(&w.h, w.keys(), &w.origins, &mreq(&slug, &t, ManageAction::Delete, None)));
    assert_eq!(again, Err(RefusalCode::PageGone));
}

#[test]
fn set_expiry_and_claim_check() {
    let w = World::new();
    let (slug, t) = make_link(&w, "load-new", Expiry::Keep, None);
    let a = block_on(manage(&w.h, w.keys(), &w.origins, &mreq(&slug, &t, ManageAction::SetExpiry, Some(Expiry::Day))))
        .unwrap();
    assert_eq!(a.expires_at, Some(w.h.now.get() + DAY));
    assert_eq!(a.choices, Expiry::ALL.to_vec());
    let c = block_on(manage(&w.h, w.keys(), &w.origins, &mreq(&slug, &t, ManageAction::ClaimCheck, None))).unwrap();
    assert!(c.ok);
    w.h.advance(DAY);
    // Past its expiry the page reads as expired, before any sweep.
    assert_eq!(page(&w, &slug).unwrap().state, DetState::Expired);
    let c = block_on(manage(&w.h, w.keys(), &w.origins, &mreq(&slug, &t, ManageAction::ClaimCheck, None)));
    assert_eq!(c, Err(RefusalCode::PageGone));
}

// ------------------------------------------------------------------ owners

fn oreq(action: OwnerAction, slug: &str, t: Option<&str>) -> OwnerRequest {
    OwnerRequest { action, slug: slug.into(), manage: t.map(Into::into) }
}

fn own(w: &World, who: u8, r: OwnerRequest) -> Result<OwnerAnswer, RefusalCode> {
    block_on(owner(&w.h, w.keys(), &w.origins, &principal(who), &r))
}

#[test]
fn peek_and_claim_answer_the_id_and_a_bookmark_never_deletes() {
    let w = World::new();
    let (slug, t) = make_link(&w, "load-new", Expiry::Keep, Some(principal(1)));
    let id = IdH(id_of(&w, &slug));
    // Account 2 peeks with the token: another account owns it.
    let Ok(OwnerAnswer::Peek(p)) = own(&w, 2, oreq(OwnerAction::Peek, &slug, Some(&t))) else { panic!() };
    assert_eq!((p.id_h, p.manage_ok, p.owner), (id, true, OwnerRelation::Other));
    assert_eq!(p.summary.band, result("x").band);
    // Its claim becomes a bookmark.
    let Ok(OwnerAnswer::Claim(c)) = own(&w, 2, oreq(OwnerAction::Claim, &slug, Some(&t))) else { panic!() };
    assert_eq!((c.id_h, c.is_owner, c.newly_owned), (id, false, false));
    // Bookmark then delete, bookmark then release: refused, nothing moves.
    let before = w.h.dump();
    for action in [OwnerAction::Delete, OwnerAction::Release] {
        let Ok(OwnerAnswer::Done(d)) = own(&w, 2, oreq(action, &slug, None)) else { panic!() };
        assert_eq!((d.ok, d.code), (false, Some(RefusalCode::NotOwner)));
    }
    assert_eq!(w.h.dump(), before);
    assert_eq!(w.h.metric_count("owner", "skipped"), 2);
    // The owner releases it; account 2's claim then owns it.
    let Ok(OwnerAnswer::Done(d)) = own(&w, 1, oreq(OwnerAction::Release, &slug, None)) else { panic!() };
    assert!(d.ok);
    let Ok(OwnerAnswer::Claim(c)) = own(&w, 2, oreq(OwnerAction::Claim, &slug, Some(&t))) else { panic!() };
    assert_eq!((c.is_owner, c.newly_owned), (true, true));
    let Ok(OwnerAnswer::Claim(c)) = own(&w, 2, oreq(OwnerAction::Claim, &slug, Some(&t))) else { panic!() };
    assert_eq!((c.is_owner, c.newly_owned), (true, false), "a second claim keeps the first");
    let via: String = w.h.db.query_row("SELECT owner_via FROM detections", [], |r| r.get(0)).unwrap();
    assert_eq!(via, "claimed");
    // A claim without the token is refused.
    assert_eq!(own(&w, 3, oreq(OwnerAction::Claim, &slug, None)), Err(RefusalCode::ManageInvalid));
    // The owner deletes it.
    let Ok(OwnerAnswer::Done(d)) = own(&w, 2, oreq(OwnerAction::Delete, &slug, None)) else { panic!() };
    assert!(d.ok);
    assert_eq!(state_of(&w, &slug), "deleted");
    assert_eq!(own(&w, 2, oreq(OwnerAction::Peek, &slug, Some(&t))), Err(RefusalCode::PageGone));
}

#[test]
fn owner_routes_refuse_public_slugs_before_any_read() {
    let w = World::new();
    w.h.exec(
        "INSERT INTO detections (id_h, public_slug, kind, platform, state, created_at, updated_at) \
VALUES (x'00', 'x4m8q2t', 'public_url', 'x', 'live', 1, 1)",
    );
    let before = (w.h.statements.get(), w.h.doc_reads.get(), w.h.dump());
    for slug in ["x4m8q2t", "k7m2xq9", "X4M8Q2T", "nope"] {
        for action in OwnerAction::ALL {
            assert_eq!(own(&w, 1, oreq(*action, slug, Some("t"))), Err(RefusalCode::NotFound), "{slug} {action}");
        }
    }
    assert_eq!((w.h.statements.get(), w.h.doc_reads.get(), w.h.dump()), before, "no read and no write");
}

// ------------------------------------------------------------------ re-run

struct FakeScorer<'a> {
    h: &'a Harness,
    during: Option<&'static str>,
}

impl Scorer for FakeScorer<'_> {
    async fn score(&self, _text: &str) -> Result<StoredResult, RefusalCode> {
        if let Some(sql) = self.during {
            self.h.exec(sql);
        }
        Ok(result("load-new"))
    }
}

fn rerun(w: &World, slug: &str, during: Option<&'static str>) -> Result<(String, String), RefusalCode> {
    let s = FakeScorer { h: &w.h, during };
    block_on(rerun_secret(&w.h, w.keys(), slug, &w.serving, &s)).map(|a| (a.slug, a.manage.unwrap()))
}

#[test]
fn a_secret_rerun_per_state_and_kind() {
    let w = World::new();
    let (cur, _) = make_link(&w, "load-new", Expiry::Keep, None);
    assert_eq!(rerun(&w, &cur, None), Err(RefusalCode::RerunNotNeeded));
    let (old, _) = make_link(&w, "load-old", Expiry::Week, Some(principal(1)));
    let (copy, copy_t) = rerun(&w, &old, None).unwrap();
    assert_ne!(copy, old);
    let (copy_of, latest, exp, owner): (Vec<u8>, i64, i64, Option<Vec<u8>>) =
        w.h.db
            .query_row(
                "SELECT copy_of, expiry_cap, expires_at, owner_h FROM detections WHERE id_h = ?1",
                [id_of(&w, &copy).to_vec()],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .unwrap();
    assert_eq!(copy_of, id_of(&w, &old).to_vec());
    assert_eq!(latest, exp, "the copy's expiry is the original's, and its latest allowed one");
    assert!(owner.is_none(), "a copy is owned by nobody");
    let cp = page(&w, &copy).unwrap();
    assert_eq!(cp.text.as_ref().unwrap().body.as_deref(), Some(canon::canon_v1(TEXT).as_ref()));
    assert!(!cp.serving.differs_from_run);
    // A copy offers only the expiry choices that end by its latest allowed one.
    let a = block_on(manage(
        &w.h,
        w.keys(),
        &w.origins,
        &mreq(&copy, &copy_t, ManageAction::SetExpiry, Some(Expiry::Keep)),
    ))
    .unwrap();
    assert_eq!(a.expires_at, Some(latest as u64));
    assert_eq!(a.choices, vec![Expiry::Day, Expiry::Week]);
    // States.
    for (sql, want) in [
        ("UPDATE detections SET state = 'disabled' WHERE id_h = ?1", RefusalCode::RerunBlocked),
        ("UPDATE detections SET state = 'removed' WHERE id_h = ?1", RefusalCode::RerunBlocked),
        ("UPDATE detections SET state = 'failed' WHERE id_h = ?1", RefusalCode::RerunUnavailable),
        ("UPDATE detections SET state = 'pending' WHERE id_h = ?1", RefusalCode::RerunUnavailable),
        ("UPDATE detections SET kind = 'private_paid' WHERE id_h = ?", RefusalCode::RerunUnavailable),
        ("UPDATE detections SET state = 'deleted', kind = 'private_text' WHERE id_h = ?1", RefusalCode::RerunGone),
    ] {
        let (s, _) = make_link(&w, "load-old", Expiry::Keep, None);
        w.h.db.execute(sql, [id_of(&w, &s).to_vec()]).unwrap();
        let reads = w.h.doc_reads.get();
        assert_eq!(rerun(&w, &s, None), Err(want), "{sql}");
        assert_eq!(w.h.doc_reads.get(), reads, "a refused re-run reads no document");
    }
    assert_eq!(rerun(&w, &"0".repeat(20), None), Err(RefusalCode::RerunGone));
    assert_eq!(rerun(&w, "k7m2xq9", None), Err(RefusalCode::NotFound));
}

#[test]
fn a_removal_or_delete_during_the_scan_wins() {
    for (during, want) in [
        ("UPDATE detections SET state = 'disabled'", RefusalCode::RerunBlocked),
        ("UPDATE detections SET state = 'deleted', manage_h = NULL", RefusalCode::RerunGone),
        ("UPDATE detections SET expires_at = 1", RefusalCode::RerunGone),
    ] {
        let w = World::new();
        let (old, _) = make_link(&w, "load-old", Expiry::Keep, None);
        assert_eq!(rerun(&w, &old, Some(during)), Err(want), "{during}");
        assert_eq!(w.h.count("SELECT count(*) FROM detections"), 1, "no copy row");
        assert_eq!(w.h.docs.borrow().len(), 1, "the copy's document was deleted");
    }
}

#[test]
fn a_family_ends_with_its_root() {
    let w = World::new();
    let (root, root_t) = make_link(&w, "load-old", Expiry::Keep, None);
    let (c1, c1_t) = rerun(&w, &root, None).unwrap();
    // A copy of the copy belongs to the same root.
    w.h.exec("UPDATE detections SET package_load = 'load-old'");
    let (c2, _) = rerun(&w, &c1, None).unwrap();
    let root_id = id_of(&w, &root).to_vec();
    assert_eq!(w.h.count(&format!("SELECT count(*) FROM detections WHERE copy_of = x'{}'", hex::encode(&root_id))), 2);
    // Deleting a copy ends only that copy.
    block_on(manage(&w.h, w.keys(), &w.origins, &mreq(&c1, &c1_t, ManageAction::Delete, None))).unwrap();
    assert_eq!(
        (state_of(&w, &root), state_of(&w, &c1), state_of(&w, &c2)),
        ("live".into(), "deleted".into(), "live".into())
    );
    // Deleting the root ends every live copy, documents included.
    block_on(manage(&w.h, w.keys(), &w.origins, &mreq(&root, &root_t, ManageAction::Delete, None))).unwrap();
    assert_eq!(state_of(&w, &c2), "deleted");
    assert!(w.h.docs.borrow().is_empty());
}

#[test]
fn the_sweep_expires_documents_first_and_roots_take_their_copies() {
    let w = World::new();
    let (root, _) = make_link(&w, "load-old", Expiry::Day, None);
    let (copy, _) = rerun(&w, &root, None).unwrap();
    let (keep, _) = make_link(&w, "load-new", Expiry::Keep, None);
    // The root's expiry was shortened after the copy was made.
    w.h.exec(&format!(
        "UPDATE detections SET expires_at = expires_at - 1000 WHERE id_h = x'{}'",
        hex::encode(id_of(&w, &root))
    ));
    w.h.advance(DAY);
    // A failing R2 delete leaves the rows as they were.
    w.h.fail_doc_delete.set(true);
    assert!(block_on(cron::sweep_round(&w.h, w.h.now.get())).is_err());
    assert_eq!(state_of(&w, &root), "live");
    w.h.fail_doc_delete.set(false);
    let h = block_on(cron::hourly(&w.h, &w.origins, true)).unwrap();
    assert_eq!(h.expired, 2);
    assert_eq!(
        (state_of(&w, &root), state_of(&w, &copy), state_of(&w, &keep)),
        ("expired".into(), "expired".into(), "live".into())
    );
    assert_eq!(w.h.docs.borrow().len(), 1);
    let p = page(&w, &root).unwrap();
    assert_eq!(p.http_status(), 410);
}

#[test]
fn the_backstop_deletes_orphans_only() {
    let w = World::new();
    let (live, _) = make_link(&w, "load-new", Expiry::Keep, None);
    let (gone, _) = make_link(&w, "load-new", Expiry::Keep, None);
    w.h.exec(&format!("UPDATE detections SET state = 'deleted' WHERE id_h = x'{}'", hex::encode(id_of(&w, &gone))));
    // A document with no row at all, and a young one (a link being made).
    block_on(slop_ninja_detect_worker::db::Io::doc_put(&w.h, &format!("p/{}", "ab".repeat(32)), vec![1])).unwrap();
    block_on(slop_ninja_detect_worker::db::Io::doc_put(&w.h, &format!("j/{}/s1", "cd".repeat(32)), vec![1])).unwrap();
    w.h.advance(2 * 3_600_000);
    block_on(slop_ninja_detect_worker::db::Io::doc_put(&w.h, &format!("p/{}", "ef".repeat(32)), vec![1])).unwrap();
    w.h.list_page.set(1);
    let h = block_on(cron::hourly(&w.h, &w.origins, true)).unwrap();
    let keys: Vec<String> = w.h.docs.borrow().keys().cloned().collect();
    assert!(keys.contains(&secret_doc_key(&id_of(&w, &live))));
    assert!(keys.contains(&format!("p/{}", "ef".repeat(32))), "young objects stay");
    // One page of one object per listing call: five calls this run, so the
    // cursor is kept for the next run.
    let ck: i64 = w.h.count("SELECT count(*) FROM schema_info WHERE k LIKE 'cron.backstop.%'");
    assert!(ck <= 1);
    let h2 = block_on(cron::hourly(&w.h, &w.origins, true)).unwrap();
    assert_eq!(h.orphans + h2.orphans, 3);
    let keys: Vec<String> = w.h.docs.borrow().keys().cloned().collect();
    assert_eq!(keys.len(), 2, "{keys:?}");
}

// ------------------------------------------------------------------ states

#[test]
fn states_report_in_order_with_expiry_on_read() {
    let w = World::new();
    let (a, _) = make_link(&w, "l", Expiry::Day, None);
    let (b, _) = make_link(&w, "l", Expiry::Keep, None);
    w.h.advance(DAY);
    let ids = vec![IdH(id_of(&w, &b)), IdH([9; 32]), IdH(id_of(&w, &a))];
    let s = block_on(states(&w.h, &StatesRequest { id_h: ids.clone() })).unwrap();
    let got: Vec<_> = s.states.iter().map(|e| (e.id_h, e.state)).collect();
    assert_eq!(got, vec![(ids[0], Some(DetState::Live)), (ids[1], None), (ids[2], Some(DetState::Expired))]);
    let many = StatesRequest { id_h: vec![IdH([1; 32]); 91] };
    assert_eq!(block_on(states(&w.h, &many)), Err(RefusalCode::TooLarge));
    let ninety = StatesRequest { id_h: vec![IdH([1; 32]); 90] };
    assert_eq!(block_on(states(&w.h, &ninety)).unwrap().states.len(), 90);
}

// ------------------------------------------------------------------- queue

#[test]
fn an_owner_purge_deletes_or_releases_only_that_owners_pages() {
    let w = World::new();
    let (mine, _) = make_link(&w, "l", Expiry::Keep, Some(principal(1)));
    let (removed, _) = make_link(&w, "l", Expiry::Keep, Some(principal(1)));
    let (theirs, _) = make_link(&w, "l", Expiry::Keep, Some(principal(2)));
    w.h.exec(&format!("UPDATE detections SET state = 'removed' WHERE id_h = x'{}'", hex::encode(id_of(&w, &removed))));
    let m = QueueMessage::new(Job::OwnerPurge { user_id: UserId([1; 16]), mode: PurgeMode::Delete });
    assert_eq!(block_on(consume(&w.h, &w.store, &m)), Ack::Done);
    assert_eq!(state_of(&w, &mine), "deleted");
    assert_eq!(state_of(&w, &removed), "removed", "a legal state stands");
    assert_eq!(state_of(&w, &theirs), "live");
    assert_eq!(w.h.count("SELECT count(*) FROM detections WHERE owner_h IS NOT NULL"), 1, "only the other account's");
    assert!(w.h.doc(&secret_doc_key(&id_of(&w, &mine))).is_none());
    // A redelivery finds nothing left.
    assert_eq!(block_on(consume(&w.h, &w.store, &m)), Ack::Done);
    let r = QueueMessage::new(Job::OwnerPurge { user_id: UserId([2; 16]), mode: PurgeMode::Release });
    assert_eq!(block_on(consume(&w.h, &w.store, &r)), Ack::Done);
    assert_eq!(state_of(&w, &theirs), "live");
    assert_eq!(w.h.count("SELECT count(*) FROM detections WHERE owner_h IS NOT NULL"), 0);
}

#[test]
fn a_merge_moves_pages_and_strikes() {
    let w = World::new();
    let (a, _) = make_link(&w, "l", Expiry::Keep, Some(principal(1)));
    let (from, to) = (w.store.owner_h(&[1; 16]), w.store.owner_h(&[2; 16]));
    w.h.db
        .execute(
            "INSERT INTO strikes (owner_h, case_id, at, owner_via) VALUES (?1, 'td-1', 1, 'created'), (?2, 'td-1', 1, 'created'), (?1, 'td-2', 1, 'created')",
            [from.to_vec(), to.to_vec()],
        )
        .unwrap();
    let m = QueueMessage::new(Job::OwnerRekey { from_user_id: UserId([1; 16]), to_user_id: UserId([2; 16]) });
    assert_eq!(block_on(consume(&w.h, &w.store, &m)), Ack::Done);
    let o: Vec<u8> = w.h.db.query_row("SELECT owner_h FROM detections", [], |r| r.get(0)).unwrap();
    assert_eq!(o, to.to_vec());
    assert_eq!(w.h.count("SELECT count(*) FROM strikes"), 2, "a case of both accounts keeps one strike");
    let _ = a;
}

#[test]
fn sitemaps_purges_and_messages_of_other_flows() {
    let w = World::new();
    let m = QueueMessage::new(Job::SitemapDirty { shard: 3 });
    assert_eq!(block_on(consume(&w.h, &w.store, &m)), Ack::Done);
    assert_eq!(w.h.count("SELECT count(*) FROM sitemap_shards WHERE shard = 3 AND dirty = 1"), 1);
    let p = QueueMessage::new(Job::Purge { urls: vec!["https://site.example/d/k7m2xq9".into()] });
    assert_eq!(block_on(consume(&w.h, &w.store, &p)), Ack::Done, "dropped without a token");
    w.h.purge_token.set(true);
    assert_eq!(block_on(consume(&w.h, &w.store, &p)), Ack::Done);
    assert_eq!(w.h.purged.borrow().len(), 1);
    let x = QueueMessage::new(Job::RerunOp { source_id: 1 });
    assert!(matches!(block_on(consume(&w.h, &w.store, &x)), Ack::Retry(_)));
}

#[test]
fn a_legal_notice_goes_to_the_relay_with_public_slugs_only() {
    let w = World::new();
    let (secret, _) = make_link(&w, "l", Expiry::Keep, None);
    w.h.exec(
        "INSERT INTO detections (id_h, public_slug, kind, platform, state, created_at, updated_at) \
VALUES (x'01', 'k7m2xq9', 'public_url', 'web', 'live', 1, 1); \
INSERT INTO cases (case_id, status_h, kind, state, received_at, updated_at, notice_key) \
VALUES ('td-abcd1234', x'02', 'copyright', 'received', 5, 5, 'c/td-abcd1234'); \
INSERT INTO case_targets VALUES ('td-abcd1234', x'01');",
    );
    w.h.db.execute("INSERT INTO case_targets VALUES ('td-abcd1234', ?1)", [id_of(&w, &secret).to_vec()]).unwrap();
    let notice = slop_ninja_detect_worker::cases::Notice {
        v: slop_ninja_detect_worker::cases::NOTICE_V,
        track: slop_ninja_detect_page::codes::CaseKind::Copyright,
        target: "https://site.example/d/k7m2xq9".into(),
        secret: false,
        fields: [("work".to_owned(), "a poem".to_owned())].into_iter().collect(),
        received_at: 5,
        signed_in: false,
    };
    let sealed = keys::seal_with(
        w.store.cases_key(),
        &[5; 12],
        slop_ninja_detect_worker::queue::CASE_AAD,
        &serde_json::to_vec(&notice).unwrap(),
    );
    block_on(slop_ninja_detect_worker::db::Io::doc_put(&w.h, "c/td-abcd1234", sealed)).unwrap();
    let m = QueueMessage::new(Job::LegalNotice {
        case_id: "td-abcd1234".into(),
        kind: slop_ninja_detect_page::codes::CaseKind::Copyright,
    });
    assert_eq!(block_on(consume(&w.h, &w.store, &m)), Ack::Done);
    let body: serde_json::Value = serde_json::from_slice(&w.h.relay.borrow()[0]).unwrap();
    assert_eq!(body["schema"], "slop_ninja.detect.legal_notice/v1");
    let mut targets: Vec<String> =
        body["targets"].as_array().unwrap().iter().map(|v| v.as_str().unwrap().to_owned()).collect();
    targets.sort();
    assert_eq!(targets, vec!["k7m2xq9".to_owned(), "private detection".to_owned()]);
    assert_eq!(body["notice"]["fields"]["work"], "a poem");
    assert!(!String::from_utf8_lossy(&w.h.relay.borrow()[0]).contains(&secret), "no secret slug");
}

// ---------------------------------------------------------------- ops, maps

use slop_ninja_detect_worker::store_ops::{self, ExampleSlot};

/// Runs an op's statements after its journal row, as the op route does.
fn apply(w: &World, stmts: Vec<slop_ninja_detect_worker::db::Stmt>) -> Vec<slop_ninja_detect_worker::db::Done> {
    let mut batch = vec![slop_ninja_detect_worker::db::Stmt::new(
        "INSERT INTO op_journal (at, actor, op) VALUES (1, 'test', 'op')",
        Vec::new(),
    )];
    batch.extend(stmts);
    block_on(slop_ninja_detect_worker::db::Sql::batch(&w.h, batch)).unwrap()
}

#[test]
fn switch_ops_take_effect_on_the_next_request() {
    let w = World::new();
    apply(&w, store_ops::set_switch("DETECT_PAGES", "off", 5).unwrap());
    let (slug, _) = make_link(&w, "l", Expiry::Keep, None);
    assert_eq!(page(&w, &slug), Err(RefusalCode::PagesOff));
    apply(&w, store_ops::set_switch("DETECT_PAGES", "on", 6).unwrap());
    assert!(page(&w, &slug).is_ok());
    assert_eq!(w.h.count("SELECT journal_seq FROM switches WHERE name = 'DETECT_PAGES'"), 2);
    assert_eq!(
        store_ops::set_switch("auth.email", "off", 1).err(),
        Some(RefusalCode::OpMalformed),
        "an account switch"
    );
    assert_eq!(store_ops::set_switch("DETECT_PAGES", "maybe", 1).err(), Some(RefusalCode::OpMalformed));
    // DETECT_CARDS = generic drops the ninja on the next read.
    apply(&w, store_ops::set_switch("DETECT_CARDS", "generic", 7).unwrap());
    assert!(page(&w, &slug).unwrap().ninja.is_none());
    let st = block_on(store_ops::status(&w.h)).unwrap();
    assert!(st.switches.contains(&("DETECT_CARDS".to_owned(), "generic".to_owned())));
    assert_eq!(st.detections.iter().fold(0, |n, x| n + x.2), 1);
}

#[test]
fn delete_private_ends_a_page_and_its_copies() {
    let w = World::new();
    let (root, _) = make_link(&w, "load-old", Expiry::Keep, None);
    let (copy, _) = rerun(&w, &root, None).unwrap();
    let id = IdH(id_of(&w, &root));
    let done = apply(&w, store_ops::delete_private(&id, w.h.now.get()));
    assert_eq!(done[1].changes, 1);
    let docs = store_ops::delete_private_docs(&id, &done[2]);
    assert_eq!(docs.len(), 2);
    assert_eq!((state_of(&w, &root), state_of(&w, &copy)), ("deleted".into(), "deleted".into()));
}

#[test]
fn examples_purges_and_index_overrides() {
    let w = World::new();
    assert!(block_on(reads::examples(&w.h, w.keys(), &w.serving)).unwrap().examples.is_empty(), "empty until set");
    let ex = fixture("28-example");
    block_on(slop_ninja_detect_worker::db::Io::doc_put(&w.h, "x/examples/v1/1", serde_json::to_vec(&ex).unwrap()))
        .unwrap();
    apply(
        &w,
        store_ops::set_examples(&[ExampleSlot::Synthetic { slot: 1, doc_key: "x/examples/v1/1".into() }]).unwrap(),
    );
    let all = block_on(reads::examples(&w.h, w.keys(), &w.serving)).unwrap();
    assert_eq!(all.examples.len(), 1);
    assert_eq!(all.examples[0].page, ex);
    assert_eq!(block_on(reads::example(&w.h, w.keys(), &w.serving, 1)).unwrap(), ex);
    assert_eq!(block_on(reads::example(&w.h, w.keys(), &w.serving, 2)), Err(RefusalCode::NotFound));
    assert!(store_ops::set_examples(&[ExampleSlot::Synthetic { slot: 5, doc_key: "x/examples/v1/5".into() }]).is_err());
    apply(&w, store_ops::set_examples(&[]).unwrap());
    assert!(block_on(reads::examples(&w.h, w.keys(), &w.serving)).unwrap().examples.is_empty());
    // Purges name only pages on the two origins.
    assert!(store_ops::purge(&w.origins, &["https://site.example/d/k7m2xq9".into()]).is_ok());
    assert!(store_ops::purge(&w.origins, &["https://elsewhere.example/d/k7m2xq9".into()]).is_err());
    assert!(store_ops::purge(&w.origins, &["https://site.example.evil/x".into()]).is_err());
    assert!(store_ops::index_force("not-a-slug", None).is_err());
}

fn public_row(w: &World, slug: &str, seq: u64, state: &str, platform: &str, reputation: &str, force: Option<&str>) {
    let id = keys::public_id_h(slug);
    w.h.db
        .execute(
            "INSERT INTO sources (canon_key, canon_url, host, adapter, state, reputation, rank_list_v) \
VALUES (?1, 'https://a.example/x', 'a.example', 'web', 'ok', ?2, 'v1')",
            rusqlite::params![id.to_vec(), reputation],
        )
        .unwrap();
    let source_id = w.h.db.last_insert_rowid();
    // A long-form run (index gate 1), so the source's own gates decide.
    w.h.db
        .execute(
            "INSERT INTO runs (run_id, package_load, package_pub, package_label, policy_v, canon_v, display_v, \
evidence_id, band, words, total_words, cut, tokens, scored_at, doc_key, doc_bytes) VALUES (?1, 'l', 'p', \
'span package d24', 'long-v1', 'canon_v1', 'display_v1', 'e', 'strong', 300, 300, 'none', 400, 1, 'r/1', 1)",
            rusqlite::params![seq as i64],
        )
        .unwrap();
    w.h.db
        .execute(
            "INSERT INTO detections (id_h, public_slug, kind, platform, state, created_at, updated_at, source_id, sitemap_seq, index_force, run_id) \
VALUES (?1, ?2, 'public_url', ?3, ?4, 1, 1795169040000, ?5, ?6, ?7, ?6)",
            rusqlite::params![id.to_vec(), slug, platform, state, source_id, seq as i64, force],
        )
        .unwrap();
}

#[test]
fn sitemap_shards_list_indexable_public_pages_only() {
    let w = World::new();
    public_row(&w, "k7m2xq9", 1, "live", "web", "ranked", None);
    public_row(&w, "p4r8tz2", 2, "live", "web", "unranked", None);
    public_row(&w, "a1b2c3d", 3, "live", "web", "unranked", Some("on"));
    public_row(&w, "e4f5g6h", 4, "removed", "web", "ranked", None);
    public_row(&w, "j7k8m9n", 5, "live", "bluesky", "ranked", None);
    public_row(&w, "q2r3s4t", 6, "live", "web", "ranked", Some("off"));
    let (secret, _) = make_link(&w, "l", Expiry::Keep, None);
    let m = QueueMessage::new(Job::SitemapDirty { shard: 0 });
    assert_eq!(block_on(consume(&w.h, &w.store, &m)), Ack::Done);
    let h = block_on(cron::hourly(&w.h, &w.origins, true)).unwrap();
    assert_eq!(h.shards, 1);
    let xml = String::from_utf8(block_on(reads::sitemap_shard(&w.h, 0)).unwrap()).unwrap();
    assert!(xml.contains("<loc>https://site.example/d/k7m2xq9</loc>"));
    assert!(xml.contains("<loc>https://site.example/d/a1b2c3d</loc>"), "index_force on");
    for absent in ["p4r8tz2", "e4f5g6h", "j7k8m9n", "q2r3s4t", secret.as_str()] {
        assert!(!xml.contains(absent), "{absent}");
    }
    assert_eq!(w.h.count("SELECT dirty FROM sitemap_shards WHERE shard = 0"), 0);
    // DETECT_INDEX off: shards rebuild empty.
    block_on(consume(&w.h, &w.store, &m));
    block_on(cron::hourly(&w.h, &w.origins, false)).unwrap();
    let xml = String::from_utf8(block_on(reads::sitemap_shard(&w.h, 0)).unwrap()).unwrap();
    assert!(!xml.contains("<loc>"));
    // An index override marks the page's shard.
    apply(&w, store_ops::index_force("k7m2xq9", Some(slop_ninja_detect_page::codes::IndexForce::Off)).unwrap());
    assert_eq!(w.h.count("SELECT dirty FROM sitemap_shards WHERE shard = 0"), 1);
}

#[test]
fn the_removal_record_names_public_pages_and_counts_secret_ones() {
    let w = World::new();
    w.h.exec(
        "INSERT INTO removals (at, public_slug, scope, basis, jurisdiction) VALUES \
(1795255440000, 'k7m2xq9', 'excerpt', 'copyright', 'US'), (1795255440000, NULL, 'page', 'illegal', 'EU'), \
(1795341840000, NULL, 'page', 'copyright', 'US')",
    );
    let r = block_on(reads::removals(&w.h)).unwrap();
    assert_eq!(r.entries.len(), 1);
    assert_eq!(r.entries[0].public_slug, "k7m2xq9");
    assert_eq!(r.secret_monthly.iter().fold(0, |n, m| n + m.count), 2);
    assert!(r.secret_monthly.iter().all(|m| m.month.len() == 7));
}

#[test]
fn cards_follow_their_page() {
    let w = World::new();
    let (slug, t) = make_link(&w, "l", Expiry::Keep, None);
    let c = block_on(reads::card(&w.h, w.keys(), &w.serving, &slug)).unwrap();
    assert!(!serde_json::to_string(&c).unwrap().contains(&slug), "a secret card holds no slug");
    block_on(manage(&w.h, w.keys(), &w.origins, &mreq(&slug, &t, ManageAction::Delete, None))).unwrap();
    assert_eq!(block_on(reads::card(&w.h, w.keys(), &w.serving, &slug)), Err(RefusalCode::PageGone));
    assert_eq!(block_on(reads::card(&w.h, w.keys(), &w.serving, "nope")), Err(RefusalCode::NotFound));
}

#[test]
fn a_preview_writes_nothing_and_keeps_its_seal() {
    let w = World::new();
    let s = sealed(&w, TEXT, "load-new");
    let before = w.h.dump();
    let p = block_on(reads::preview(&w.h, &w.serving, TEXT, Some(&s))).unwrap();
    assert_eq!(p.kind.as_str(), "unstored");
    assert_eq!(block_on(reads::preview(&w.h, &w.serving, "another text", Some(&s))), Err(RefusalCode::SealInvalid));
    assert_eq!(w.h.dump(), before);
    assert!(
        block_on(share(&w.h, w.keys(), &req(TEXT, false, None), Some(s), None)).is_ok(),
        "the seal still makes a link"
    );
}

#[test]
fn a_public_page_s_poll_answers_its_state_and_nothing_else() {
    let w = World::new();
    public_row(&w, "k7m2xq9", 1, "pending", "web", "ranked", None);
    public_row(&w, "p4r8tz2", 2, "failed", "web", "ranked", None);
    w.h.exec("UPDATE detections SET refusal = 'robots' WHERE public_slug = 'p4r8tz2'");
    let a = block_on(reads::state(&w.h, "k7m2xq9")).unwrap();
    assert_eq!((a.state, a.lease_ended, a.code), (DetState::Pending, true, None));
    let a = block_on(reads::state(&w.h, "p4r8tz2")).unwrap();
    assert_eq!((a.state, a.code), (DetState::Failed, Some(RefusalCode::Robots)));
    // A secret slug, an unknown one and a malformed one are all not found.
    let (secret, _) = make_link(&w, "load-new", Expiry::Keep, None);
    for slug in [secret.as_str(), "z9y8x7w", "nope"] {
        assert_eq!(block_on(reads::state(&w.h, slug)), Err(RefusalCode::NotFound), "{slug}");
    }
}

/// The seam between the free check and the store: a result seal made by the
/// seal module, as the score route makes it, opens into a link whose page
/// prints the answer's values, and a seal of another package does not.
#[test]
fn a_free_check_s_seal_becomes_a_link() {
    use slop_ninja_detect_worker::answer::{PackageLine, ResultFull, Span};
    use slop_ninja_detect_worker::{handoff, seal};
    let w = World::new();
    let secret: [u8; 32] = *b"store secret bytes, for tests ok";
    let now = 1_795_169_040_000;
    // A short text's line, as the score route writes it: no percentages.
    let full = ResultFull {
        band: "strong".into(),
        confidence_tenths: None,
        confidence_text: None,
        share_text: None,
        share_pct: None,
        words: 52,
        total_words: 52,
        cut: "none".into(),
        scored_end: TEXT.trim_end().encode_utf16().count(),
        tokens: 70,
        length: "50_99".into(),
        spans: Some(vec![Span { start: 14, end: 200, words: 36, confidence: None }]),
        spans_note: None,
        markers: vec![],
        invisible: false,
        package: PackageLine { digest: DIGEST.into(), publication_pending: true },
    };
    let payload = |load: &str| seal::ResultPayload {
        day: seal::day_of(now),
        text_sha256: seal::text_sha256(TEXT),
        scored_end: full.scored_end,
        cut: "none".into(),
        package_load: load.into(),
        policy_v: seal::POLICY_V.into(),
        result_full: full.clone(),
        issued_ms: now,
    };
    let make = |load: &str, nonce: u8| {
        let s = seal::seal(&secret, seal::Purpose::Result, seal::day_of(now), [nonce; 12], &payload(load)).unwrap();
        handoff::sealed(seal::open_result(&secret, now + 1, &s, TEXT).unwrap(), &w.serving)
    };
    assert_eq!(make("load-old", 1).unwrap_err(), RefusalCode::SealInvalid, "another package scored it");
    let sealed = make("load-new", 2).unwrap();
    assert_eq!(sealed.text_sha256, canon::text_sha256(TEXT));
    let a = block_on(share(&w.h, w.keys(), &req(TEXT, false, None), Some(sealed), None)).unwrap();
    let p = page(&w, &a.slug).unwrap();
    let run = p.run.as_ref().unwrap();
    assert_eq!(run.short, Some(slop_ninja_detect_page::codes::ShortResult::Strong));
    assert_eq!(run.spans.len(), 1);
    assert_eq!((run.spans[0].start, run.spans[0].end), (14, 200));
    assert_eq!((run.scorer_build.as_str(), run.evidence.as_str()), ("7030a86f", "evidence-d24-web-v1"));
    assert_eq!(run.package.label, "span package d24");
    // The same seal opened again cannot make a second link.
    let again = make("load-new", 2).unwrap();
    assert_eq!(block_on(share(&w.h, w.keys(), &req(TEXT, false, None), Some(again), None)), Err(RefusalCode::SealUsed));
}
