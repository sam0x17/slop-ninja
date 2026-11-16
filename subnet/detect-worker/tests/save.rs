//! Every check's page (`permalink-policy/v1`): content-addressed short
//! slugs, collisions, the claim before the document, generations after a
//! delete, holds while a notice is reviewed, runs added under a newer
//! package, and what a dump of the store holds.

mod common;

use common::{Harness, block_on, interleave};
use slop_ninja_detect_page::api::{ManageRequest, SavedAs};
use slop_ninja_detect_page::codes::{DetState, ManageAction, OwnerAction};
use slop_ninja_detect_page::ids::UserId;
use slop_ninja_detect_page::internal::{OwnerRequest, SaveAnswer, SaveRequest};
use slop_ninja_detect_page::origins::Origins;
use slop_ninja_detect_page::page::Page;
use slop_ninja_detect_page::refusal::RefusalCode;
use slop_ninja_detect_page::sn::Principal;
use slop_ninja_detect_worker::canon;
use slop_ninja_detect_worker::doc::{SecretDoc, Serving, StoredResult};
use slop_ninja_detect_worker::keys::{Pepper, Peppers, StoreKeys};
use slop_ninja_detect_worker::manage::{OwnerAnswer, manage, owner};
use slop_ninja_detect_worker::reads;
use slop_ninja_detect_worker::save::{self, Keys, Sealed};
use slop_ninja_detect_worker::store::secret_doc_key;

const DIGEST: &str = "e2536d12dff593d7b11895b3cbf6b10232601abfbebde248325afd707722b5e8";
/// `permalink-policy/v1`'s worked example: 36 words, already canonical.
const T: &str = "The quarterly report shows steady growth across every region, with the strongest gains in the \
northern stores. Our team expects the trend to continue into next year as new locations open and online orders \
keep rising.";

struct World {
    h: Harness,
    store: StoreKeys,
    peppers: Peppers,
    origins: Origins,
    serving: Serving,
}

impl World {
    /// The worked example's pepper (0x00 to 0x1f, test only).
    fn new() -> World {
        World {
            h: Harness::new(),
            store: StoreKeys::derive(&[7u8; 32]),
            peppers: Peppers::new(vec![Pepper::new(1, core::array::from_fn(|i| i as u8))]).unwrap(),
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

fn result(load: &str, scored_at: u64) -> StoredResult {
    let p = format!("{}/../crates/detect-page/tests/fixtures/pages/15-secret-text.json", env!("CARGO_MANIFEST_DIR"));
    let page: Page = serde_json::from_str(&std::fs::read_to_string(p).unwrap()).unwrap();
    let run = page.run.unwrap();
    StoredResult {
        scored_at,
        package_load: load.to_owned(),
        package_label: if load == "load-new" { "span package d24".into() } else { "span package d23".into() },
        package_pub: DIGEST.into(),
        publication_pending: true,
        scorer_build: run.scorer_build,
        policy: run.policy,
        evidence: run.evidence,
        band: run.band,
        short: run.short,
        confidence_tenths: run.confidence_tenths,
        confidence_text: run.confidence_text,
        share_pct: run.share_pct,
        share_text: run.share_text,
        words: run.words,
        total_words: run.total_words,
        cut: run.cut,
        tokens: run.tokens,
        length: run.length,
        spans: Vec::new(),
        spans_note: run.spans_note,
        markers: Vec::new(),
        invisible: false,
        raw: None,
    }
}

fn sealed(w: &World, text: &str, load: &str) -> Sealed {
    let mut nonce = vec![0u8; 12];
    slop_ninja_detect_worker::db::Io::fill(&w.h, &mut nonce);
    Sealed { text_sha256: canon::text_sha256(text), nonce, result: result(load, w.h.now.get()) }
}

fn principal(n: u8) -> Principal {
    Principal { user_id: UserId([n; 16]), auth_time_ms: 1, flags: 0 }
}

fn save_with(w: &World, text: &str, s: Sealed, who: Option<Principal>) -> Result<SaveAnswer, RefusalCode> {
    let req = SaveRequest { text: text.into(), seal: "opened-elsewhere".into(), save: who.is_some() };
    block_on(save::save(&w.h, w.keys(), &req, Some(s), who))
}

fn save_as(w: &World, text: &str, load: &str, who: Option<Principal>) -> Result<SaveAnswer, RefusalCode> {
    save_with(w, text, sealed(w, text, load), who)
}

fn page(w: &World, slug: &str) -> Result<Page, RefusalCode> {
    block_on(reads::page(&w.h, w.keys(), &w.serving, slug))
}

fn id_of(w: &World, slug: &str) -> [u8; 32] {
    w.peppers.current().secret_id_h(slug)
}

fn delete(w: &World, slug: &str, token: &str) {
    let r = ManageRequest { slug: slug.into(), manage: token.into(), action: ManageAction::Delete };
    block_on(manage(&w.h, w.keys(), &w.origins, &r)).unwrap();
}

fn set_state(w: &World, slug: &str, state: &str) {
    w.h.db.execute("UPDATE detections SET state = ?1 WHERE id_h = ?2", (state, id_of(w, slug).to_vec())).unwrap();
}

/// The worked example, steps 1, 2, 5 and 6: the first check makes
/// `uM07zZQv`; a signed-in reader's check of the same text lands on it as a
/// bookmark; after a delete the old link stays 410 and the text's next
/// check makes generation 1, `XxXl3YyG`, with its own manage token.
#[test]
fn the_worked_example() {
    let w = World::new();
    let a = save_as(&w, T, "load-new", None).unwrap();
    assert_eq!((a.slug.as_str(), a.made), ("uM07zZQv", true));
    let token = a.manage.clone().unwrap();
    let b = save_as(&w, T, "load-new", Some(principal(2))).unwrap();
    assert_eq!((b.slug.as_str(), b.made, b.manage, b.saved), ("uM07zZQv", false, None, Some(SavedAs::Bookmark)));
    // Edge spaces and a CRLF: the same text.
    let c = save_as(&w, &format!("  {T}\r\n"), "load-new", None).unwrap();
    assert_eq!(c.slug, "uM07zZQv");
    // One comma removed: another text.
    let other = T.replacen(',', "", 1);
    assert_ne!(save_as(&w, &other, "load-new", None).unwrap().slug, "uM07zZQv");
    // The first reader deletes the page.
    delete(&w, "uM07zZQv", &token);
    assert_eq!(page(&w, "uM07zZQv").unwrap().http_status(), 410);
    // A third reader's check: generation 1, a new slug and a new token.
    let d = save_as(&w, T, "load-new", None).unwrap();
    assert_eq!((d.slug.as_str(), d.made), ("XxXl3YyG", true));
    assert!(d.manage.is_some() && d.manage.as_deref() != Some(token.as_str()));
    assert_eq!(page(&w, "uM07zZQv").unwrap().http_status(), 410, "the old link stays dead");
    assert_eq!(page(&w, "XxXl3YyG").unwrap().state, DetState::Live);
    let gen_: i64 = w.h.count("SELECT gen FROM texts WHERE gen > 0");
    assert_eq!(gen_, 1);
}

/// A candidate another text holds (here a row already holding its id) is
/// skipped one character at a time; the text keeps the longer slug, and a
/// later save of it reads its row and lands there without probing.
#[test]
fn a_collision_extends_the_slug_and_the_text_keeps_it() {
    let w = World::new();
    let p = w.peppers.current();
    let chars = p.text_slug_chars(T, 0);
    // Another page holds `uM07zZQv` (any state: a tombstone holds it too).
    w.h.db
        .execute(
            "INSERT INTO detections (id_h, pepper_v, kind, platform, state, created_at, updated_at) \
             VALUES (?1, 1, 'private_text', 'paste', 'deleted', 1, 1)",
            [p.text_id_h(&chars[..8]).to_vec()],
        )
        .unwrap();
    let a = save_as(&w, T, "load-new", None).unwrap();
    assert_eq!(a.slug, "uM07zZQve");
    let len: i64 = w.h.count("SELECT slug_len FROM texts");
    assert_eq!(len, 9);
    // The later save lands on it with no probe: one batch reads the text
    // row, and nothing is written.
    let before = (w.h.statements.get(), w.h.dump());
    let b = save_as(&w, T, "load-new", None).unwrap();
    assert_eq!((b.slug.as_str(), b.made), ("uM07zZQve", false));
    assert_eq!(w.h.dump(), before.1);
    assert!(w.h.statements.get() - before.0 <= 3, "one read batch, no probe");
    assert_eq!(page(&w, "uM07zZQve").unwrap().state, DetState::Live);
    assert_eq!(page(&w, "uM07zZQv").unwrap().http_status(), 410);
}

/// Two saves of different texts whose candidates meet: the one whose claim
/// lands second finds the slug held and takes the next length; only the
/// winner writes a document under that id.
#[test]
fn the_different_text_collision_race() {
    let w = World::new();
    let p = w.peppers.current();
    let cand = p.text_slug_chars(T, 0)[..8].to_owned();
    let id = p.text_id_h(&cand);
    // Another text's save claims `cand` just before this save's claim.
    w.h.before("INSERT INTO detections (id_h, pepper_v, kind, platform, state, state_v", move |db| {
        db.execute(
            "INSERT INTO detections (id_h, pepper_v, kind, platform, state, created_at, updated_at, seal_nonce_h) \
             VALUES (?1, 1, 'private_text', 'paste', 'pending', 1, 1, ?2)",
            (id.to_vec(), vec![0xeeu8; 32]),
        )
        .unwrap();
    });
    let a = save_as(&w, T, "load-new", None).unwrap();
    assert_eq!(a.slug, format!("{cand}e"));
    assert!(w.h.doc(&secret_doc_key(&id)).is_none(), "the loser writes nothing under the winner's id");
    assert_eq!(w.h.count("SELECT count(*) FROM texts"), 1);
}

/// Two saves of the same text at once: one page, one manage token.
#[test]
fn the_same_text_race_makes_one_page() {
    let w = World::new();
    w.h.pause_at("INSERT INTO detections (id_h, pepper_v, kind, platform, state, state_v");
    let (s1, s2) = (sealed(&w, T, "load-new"), sealed(&w, T, "load-new"));
    let req = SaveRequest { text: T.into(), seal: "x".into(), save: false };
    let (a, b) =
        interleave(save::save(&w.h, w.keys(), &req, Some(s1), None), save::save(&w.h, w.keys(), &req, Some(s2), None));
    let (a, b) = (a.unwrap(), b.unwrap());
    assert_eq!(a.slug, b.slug);
    assert_eq!([a.made, b.made].iter().filter(|m| **m).count(), 1);
    assert_eq!([a.manage.is_some(), b.manage.is_some()].iter().filter(|m| **m).count(), 1);
    assert_eq!(w.h.count("SELECT count(*) FROM detections"), 1);
    assert_eq!(w.h.docs.borrow().len(), 1);
}

/// A failed document put ends the claim and lets its seal go: no link was
/// shown, and the next save of the text (the same seal included) takes the
/// next generation.
#[test]
fn a_failed_put_tombstones_the_claim_and_the_next_save_takes_the_next_generation() {
    let w = World::new();
    let s = sealed(&w, T, "load-new");
    w.h.fail_doc_put.set(true);
    assert_eq!(save_with(&w, T, s.clone(), None), Err(RefusalCode::SaveUnavailable));
    w.h.fail_doc_put.set(false);
    let st: String = w.h.db.query_row("SELECT state FROM detections", [], |r| r.get(0)).unwrap();
    assert_eq!(st, "deleted");
    assert_eq!(w.h.count("SELECT count(*) FROM detections WHERE seal_nonce_h IS NOT NULL"), 0);
    let a = save_with(&w, T, s, None).unwrap();
    assert_eq!((a.slug.as_str(), a.made), ("XxXl3YyG", true));
    assert_eq!(page(&w, "uM07zZQv").unwrap().http_status(), 410);
}

/// A disabled page holds its text: no new page while the notice is
/// reviewed. Deleting it keeps the hold (by every delete path), so the text
/// cannot get a new link meanwhile; a removed page holds its text as long
/// as it is removed. The checker sees `text_on_hold`, which the website shows
/// as the generic unsaved line.
#[test]
fn legal_states_hold_the_text() {
    for path in ["token", "owner", "operator"] {
        let w = World::new();
        let a = save_as(&w, T, "load-new", Some(principal(1))).unwrap();
        set_state(&w, &a.slug, "disabled");
        assert_eq!(save_as(&w, T, "load-new", None), Err(RefusalCode::TextOnHold), "{path}: disabled");
        match path {
            "token" => delete(&w, &a.slug, a.manage.as_deref().unwrap()),
            "owner" => {
                let r = OwnerRequest { action: OwnerAction::Delete, slug: a.slug.clone(), manage: None };
                let Ok(OwnerAnswer::Done(d)) = block_on(owner(&w.h, w.keys(), &w.origins, &principal(1), &r)) else {
                    panic!()
                };
                assert!(d.ok);
            }
            _ => {
                let id = slop_ninja_detect_page::ids::IdH(id_of(&w, &a.slug));
                block_on(slop_ninja_detect_worker::db::Sql::batch(
                    &w.h,
                    slop_ninja_detect_worker::store_ops::delete_private(&id, w.h.now.get()),
                ))
                .unwrap();
            }
        }
        let st: String = w.h.db.query_row("SELECT state FROM detections", [], |r| r.get(0)).unwrap();
        assert_eq!(st, "deleted", "{path}");
        assert_eq!(w.h.count("SELECT count(*) FROM texts WHERE hold = 'notice'"), 1, "{path}: on hold");
        assert_eq!(save_as(&w, T, "load-new", None), Err(RefusalCode::TextOnHold), "{path}: held after the delete");
        assert_eq!(w.h.count("SELECT count(*) FROM detections"), 1, "{path}: no new page");
        // The operator's release lets the text get its next page.
        block_on(slop_ninja_detect_worker::db::Sql::run(
            &w.h,
            slop_ninja_detect_worker::store_ops::release_text(&id_of(&w, &a.slug), w.h.now.get()),
        ))
        .unwrap();
        assert_eq!(save_as(&w, T, "load-new", None).unwrap().slug, "XxXl3YyG", "{path}");
    }
    // A wrong token deletes nothing and holds nothing.
    let w = World::new();
    let a = save_as(&w, T, "load-new", None).unwrap();
    set_state(&w, &a.slug, "disabled");
    let r = ManageRequest { slug: a.slug.clone(), manage: "0".repeat(26), action: ManageAction::Delete };
    assert!(block_on(manage(&w.h, w.keys(), &w.origins, &r)).is_err());
    assert_eq!(w.h.count("SELECT count(*) FROM texts WHERE hold IS NOT NULL"), 0);
    // Deleting a live page holds nothing.
    set_state(&w, &a.slug, "live");
    delete(&w, &a.slug, a.manage.as_deref().unwrap());
    assert_eq!(w.h.count("SELECT count(*) FROM texts WHERE hold IS NOT NULL"), 0);
    // A removed page holds its text.
    let w = World::new();
    let a = save_as(&w, T, "load-new", None).unwrap();
    set_state(&w, &a.slug, "removed");
    assert_eq!(save_as(&w, T, "load-new", None), Err(RefusalCode::TextOnHold));
    assert_eq!(w.h.metric_count("store", "on_hold"), 1);
}

/// A check under a newer package adds its run to the text's page; a check
/// under the page's own package adds nothing.
#[test]
fn a_newer_packages_check_adds_its_run() {
    let w = World::new();
    w.h.purge_token.set(true);
    let a = save_as(&w, T, "load-old", None).unwrap();
    let before = page(&w, &a.slug).unwrap();
    assert!(before.serving.differs_from_run);
    w.h.advance(86_400_000);
    let b = save_as(&w, T, "load-new", None).unwrap();
    assert_eq!((b.slug.as_str(), b.made, b.updated, b.manage), (a.slug.as_str(), false, true, None));
    let p = page(&w, &a.slug).unwrap();
    assert!(!p.serving.differs_from_run);
    assert_eq!(p.history.len(), 1);
    assert_eq!(p.history[0].package.label, "span package d23");
    assert_eq!(p.history[0].scored_at, before.run.as_ref().unwrap().scored_at);
    assert_eq!(p.run.as_ref().unwrap().scored_at, before.run.as_ref().unwrap().scored_at + 86_400_000);
    assert!(p.state_v > before.state_v, "the card URL changes");
    assert_eq!(w.h.purged_tags.borrow().len(), 1, "the page's cache tag is purged");
    // The document holds both runs, oldest first.
    let doc = w.h.doc(&secret_doc_key(&id_of(&w, &a.slug))).unwrap();
    let d = SecretDoc::decode(&w.peppers.current().doc_key(&a.slug).open(&doc).unwrap()).unwrap();
    assert_eq!(d.runs.iter().map(|r| r.package_load.as_str()).collect::<Vec<_>>(), ["load-old", "load-new"]);
    // The same package again: nothing is added.
    let c = save_as(&w, T, "load-new", None).unwrap();
    assert_eq!((c.updated, c.made), (false, false));
    assert_eq!(page(&w, &a.slug).unwrap().history.len(), 1);
}

/// `permalink-policy/v1`'s var: a longer length applies to new texts only,
/// and a value outside 8 to 19 refuses every save (the check still
/// answers elsewhere).
#[test]
fn the_policy_length() {
    let w = World::new();
    let a = save_as(&w, T, "load-new", None).unwrap();
    w.h.vars.borrow_mut().insert(save::SLUG_LEN_VAR.into(), "10".into());
    let other = format!("{T} Next quarter looks the same.");
    let b = save_as(&w, &other, "load-new", None).unwrap();
    assert_eq!(b.slug.len(), 10);
    assert_eq!(save_as(&w, T, "load-new", None).unwrap().slug, a.slug, "an existing text keeps its length");
    for bad in ["7", "20", "eight", ""] {
        w.h.vars.borrow_mut().insert(save::SLUG_LEN_VAR.into(), bad.into());
        if bad.is_empty() {
            continue;
        }
        let third = format!("{T} A {bad} note.");
        assert_eq!(save_as(&w, &third, "load-new", None), Err(RefusalCode::SaveUnavailable), "{bad}");
    }
    assert!(w.h.metric_count("error", "policy") >= 3);
}

/// What a dump of D1 and R2 holds without the Worker's secrets: no text, no
/// slug, no token; the text's address is a keyed hash.
#[test]
fn a_dump_holds_keyed_hashes_only() {
    let w = World::new();
    let a = save_as(&w, T, "load-new", Some(principal(1))).unwrap();
    let dump = w.h.dump();
    assert!(!dump.contains(&a.slug) && !dump.contains(a.manage.as_deref().unwrap()));
    for word in ["quarterly", "northern", "rising"] {
        assert!(!dump.contains(word), "{word}");
    }
    let plain = hex::encode(canon::text_sha256(T));
    assert!(!dump.to_lowercase().contains(&plain), "the text's plain hash is never stored");
    let docs = w.h.docs.borrow();
    for (bytes, _) in docs.values() {
        assert!(!String::from_utf8_lossy(bytes).contains("quarterly"));
    }
}
