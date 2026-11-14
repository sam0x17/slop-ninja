//! The paid whole-document check on the detect store's own migration: the
//! quote, the start, the fenced job driver end to end, two drivers on one
//! job (side by side, and one whose lease lapsed), a cancel mid-job, a
//! section refused for good, a failed commit, the pause, the subrequest
//! calls of a long job, the sweep, and a quote whose hold was given back.
//! The ledger is a fake with the account Worker's semantics (a hold per
//! idempotency key until it ends, one end per hold); the account Worker's
//! own suite runs the real ledger.

mod common;

use std::cell::{Cell, RefCell};
use std::future::Future;
use std::pin::Pin;
use std::task::{Context, Poll, Waker};

use common::{Harness, block_on};
use slop_ninja_account_wire::{
    CommitRequest, HoldAnswer, HoldEnd, HoldEndAnswer, HoldPart, HoldRequest, OwnedAddRequest, PartSource, PeekAnswer,
    PeekRequest, ReleaseRequest,
};
use slop_ninja_detect_page::api::{JobLine, ManageRequest, QuoteRequest, StartRequest};
use slop_ninja_detect_page::codes::{Band, DetState, Expiry, Kind, ManageAction};
use slop_ninja_detect_page::ids::UserId;
use slop_ninja_detect_page::origins::Origins;
use slop_ninja_detect_page::page::Span;
use slop_ninja_detect_page::queue::{Job, QueueMessage};
use slop_ninja_detect_page::refusal::RefusalCode;
use slop_ninja_detect_page::sn::{Principal, flags};
use slop_ninja_detect_worker::answer::{PackageLine, ResultFull, SpansNote};
use slop_ninja_detect_worker::db::{DocPage, Done, Io, IoError, Sql, Stmt};
use slop_ninja_detect_worker::doc::{Serving, StoredResult};
use slop_ninja_detect_worker::keys::{Pepper, Peppers, StoreKeys};
use slop_ninja_detect_worker::paid::{
    self, Calls, Drive, JobEnd, JobSink, Ledger, SUBREQUESTS_PER_INVOCATION, ScoreFail, Scored, SectionScorer,
};
use slop_ninja_detect_worker::routes::{MAX_SECTION_BYTES, PAID_TEXT_BYTES};
use slop_ninja_detect_worker::share::Keys;
use slop_ninja_detect_worker::{canon, cut, policy, reads, seal};

const DIGEST: &str = "e2536d12dff593d7b11895b3cbf6b10232601abfbebde248325afd707722b5e8";
const LEASE: u64 = 360_000;

// ------------------------------------------------------------------ world

struct World {
    h: Harness,
    store: StoreKeys,
    peppers: Peppers,
    serving: Serving,
    secret: [u8; 32],
    origins: Origins,
}

impl World {
    fn new() -> World {
        let w = World {
            h: Harness::new(),
            store: StoreKeys::derive(&[7u8; 32]),
            peppers: Peppers::new(vec![Pepper::new(1, [3u8; 32])]).unwrap(),
            serving: Serving {
                load: "a".repeat(64),
                label: "span package d24".into(),
                published: DIGEST.into(),
                pending: true,
                scorer_build: "7030a86f".into(),
                evidence: "evidence-d24-web-v1".into(),
            },
            secret: *b"store secret bytes, for tests ok",
            origins: Origins::from_vars("https://site.example", "https://s.site.example").unwrap(),
        };
        w.h.vars.borrow_mut().insert("DETECT_PAID".into(), "on".into());
        w
    }

    fn keys(&self) -> Keys<'_> {
        Keys { store: &self.store, peppers: &self.peppers }
    }

    fn cx(&self) -> paid::Ctx<'_> {
        paid::Ctx {
            secret: &self.secret,
            keys: self.keys(),
            serving: &self.serving,
            max_text_bytes: PAID_TEXT_BYTES,
            max_section_bytes: MAX_SECTION_BYTES,
        }
    }

    /// The free check's seal of `text`, as the score route makes it.
    fn free_seal(&self, text: &str, nonce: u8) -> String {
        let canonical = canon::canon_v1(text);
        let c = cut::free_cut(&canonical, &mut |_, _| true).unwrap();
        let full = ResultFull {
            band: "unclear".into(),
            confidence_tenths: Some(655),
            confidence_text: Some("65.5%".into()),
            share_text: Some("about 12%".into()),
            share_pct: Some(12),
            words: c.scored_words,
            total_words: c.total_words,
            cut: c.kind.code().into(),
            scored_end: c.scored_end(),
            tokens: c.tokens,
            length: if c.scored_words > 450 { "451_1000".into() } else { "150_450".into() },
            spans: None,
            spans_note: Some(SpansNote::StrongBandsOnly),
            markers: vec![],
            invisible: false,
            package: PackageLine { digest: DIGEST.into(), publication_pending: true },
        };
        let now = self.h.now.get();
        let payload = seal::ResultPayload {
            day: seal::day_of(now),
            text_sha256: seal::text_sha256(text),
            scored_end: full.scored_end,
            cut: c.kind.code().into(),
            package_load: self.serving.load.clone(),
            policy_v: seal::POLICY_V.into(),
            result_full: full,
            issued_ms: now,
        };
        seal::seal(&self.secret, seal::Purpose::Result, payload.day, [nonce; 12], &payload).unwrap()
    }

    fn page(&self, slug: &str) -> Result<slop_ninja_detect_page::page::Page, RefusalCode> {
        block_on(reads::page(&self.h, self.keys(), &self.serving, slug))
    }

    fn docs_under(&self, prefix: &str) -> usize {
        self.h.docs.borrow().keys().filter(|k| k.starts_with(prefix)).count()
    }
}

fn reader(paid_preview: bool) -> Principal {
    Principal {
        user_id: UserId([0x5a; 16]),
        auth_time_ms: 1,
        flags: if paid_preview { flags::PAID_PREVIEW } else { 0 },
    }
}

/// `n` words of prose in sentences and paragraphs.
fn prose(n: usize) -> String {
    const W: &[&str] = &["the", "garden", "kept", "its", "quiet", "beds", "near", "the", "old", "wall", "and", "a"];
    let mut s = String::from("Garden notes\n\n");
    for i in 0..n {
        if i > 0 {
            s.push_str(if i % 84 == 0 { "\n\n" } else { " " });
        }
        s.push_str(W[i % W.len()]);
        if (i + 1) % 14 == 0 {
            s.push('.');
        }
    }
    s.push_str(".\r\n");
    s
}

// --------------------------------------------------------------- the fakes

/// A placed hold: its idempotency key, id, tokens and end.
type FakeHold = (String, String, u64, Option<HoldEnd>);
/// A hook run during the scorer's call of that number.
type Hook<'a> = Option<(u32, Box<dyn FnMut() + 'a>)>;

/// A ledger with the account Worker's semantics.
#[derive(Default)]
struct FakeLedger {
    credit: Cell<u64>,
    cash: Cell<bool>,
    billing_off: Cell<bool>,
    down: Cell<bool>,
    holds: RefCell<Vec<FakeHold>>,
    commits: Cell<u32>,
    releases: Cell<u32>,
    peeks: Cell<u32>,
}

impl FakeLedger {
    fn with(credit: u64) -> FakeLedger {
        let l = FakeLedger::default();
        l.credit.set(credit);
        l
    }

    fn end(&self, hold_id: &str, kind: HoldEnd) -> Result<HoldEndAnswer, RefusalCode> {
        if self.down.get() {
            return Err(RefusalCode::Unavailable);
        }
        let mut holds = self.holds.borrow_mut();
        let h = holds.iter_mut().find(|h| h.1 == hold_id).ok_or(RefusalCode::NotFound)?;
        if let Some(e) = h.3 {
            return Ok(HoldEndAnswer { hold_id: hold_id.into(), ended: e, by_this_call: false });
        }
        h.3 = Some(kind);
        match kind {
            HoldEnd::Committed => self.commits.set(self.commits.get() + 1),
            HoldEnd::Released => {
                self.releases.set(self.releases.get() + 1);
                self.credit.set(self.credit.get() + h.2);
            }
        }
        Ok(HoldEndAnswer { hold_id: hold_id.into(), ended: kind, by_this_call: true })
    }
}

impl Ledger for FakeLedger {
    async fn peek(&self, _req: &PeekRequest) -> Result<PeekAnswer, RefusalCode> {
        self.peeks.set(self.peeks.get() + 1);
        if self.billing_off.get() {
            return Err(RefusalCode::PaidOff);
        }
        Ok(PeekAnswer { plan_left: 0, api_left: 0, credit_left: self.credit.get(), paid_preview: false })
    }

    async fn hold(&self, req: &HoldRequest) -> Result<HoldAnswer, RefusalCode> {
        if self.billing_off.get() {
            return Err(RefusalCode::PaidOff);
        }
        let part = |t| vec![HoldPart { src: PartSource::Payment, id: 1, tokens: t, cash: self.cash.get() }];
        if let Some(h) = self.holds.borrow().iter().find(|h| h.0 == req.idem) {
            // An ended hold never backs another check.
            if h.3.is_some() {
                return Err(RefusalCode::QuoteInvalid);
            }
            return Ok(HoldAnswer { hold_id: h.1.clone(), parts: part(h.2) });
        }
        if req.tokens > self.credit.get() {
            return Err(RefusalCode::NotEnoughCredit);
        }
        self.credit.set(self.credit.get() - req.tokens);
        let id = format!("h{:04}", self.holds.borrow().len() + 1);
        self.holds.borrow_mut().push((req.idem.clone(), id.clone(), req.tokens, None));
        Ok(HoldAnswer { hold_id: id, parts: part(req.tokens) })
    }

    async fn commit(&self, req: &CommitRequest) -> Result<HoldEndAnswer, RefusalCode> {
        assert_eq!(req.package_pub, DIGEST);
        assert!(req.receipt_id.starts_with("pr"));
        self.end(&req.hold_id, HoldEnd::Committed)
    }

    async fn release(&self, req: &ReleaseRequest) -> Result<HoldEndAnswer, RefusalCode> {
        self.end(&req.hold_id, HoldEnd::Released)
    }
}

/// A scorer: odd calls unclear, even calls very strong with a passage of
/// the section's first 30 words. A hook may run during a given call (to
/// interleave another flow), and a call may be refused.
#[derive(Default)]
struct FakeScorer<'a> {
    calls: Cell<u32>,
    refuse_on: Cell<Option<(u32, ScoreFail)>>,
    hook: RefCell<Hook<'a>>,
    yields: Cell<bool>,
}

struct YieldOnce(bool);

impl Future for YieldOnce {
    type Output = ();
    fn poll(mut self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<()> {
        if self.0 {
            Poll::Ready(())
        } else {
            self.0 = true;
            Poll::Pending
        }
    }
}

impl SectionScorer for FakeScorer<'_> {
    async fn score(&self, text: &str, words: u32, tokens: u64, calls: &Calls) -> Result<Scored, ScoreFail> {
        if self.yields.get() {
            YieldOnce(false).await;
        }
        let n = self.calls.get() + 1;
        self.calls.set(n);
        // The gate's enter and done, and the lane.
        calls.take(3);
        assert!(words as usize >= slop_ninja_detect_worker::bound::MEASURED_MIN_WORDS && tokens <= 2048);
        let fire = matches!(&*self.hook.borrow(), Some((at, _)) if *at == n);
        if fire {
            let (_, mut f) = self.hook.borrow_mut().take().unwrap();
            f();
        }
        if let Some((at, why)) = self.refuse_on.get()
            && at == n
        {
            return Err(why);
        }
        if n.is_multiple_of(2) {
            let end = cut::word_start(text, 30).map(|b| text[..b].trim_end().encode_utf16().count()).unwrap() as u32;
            Ok(Scored {
                band: Band::VeryStrong,
                confidence_tenths: 999,
                confidence_text: "above 99.9%".into(),
                share_pct: 71,
                share_text: "about 71%".into(),
                spans: vec![Span { start: 0, end, words: 30, confidence: Some(97) }],
                tokens,
            })
        } else {
            Ok(Scored {
                band: Band::Unclear,
                confidence_tenths: 655,
                confidence_text: "65.5%".into(),
                share_pct: 12,
                share_text: "about 12%".into(),
                spans: vec![],
                tokens,
            })
        }
    }
}

#[derive(Default)]
struct Sink {
    lines: Vec<JobLine>,
    gone_after: Option<usize>,
}

impl JobSink for Sink {
    async fn line(&mut self, l: &JobLine) -> bool {
        self.lines.push(l.clone());
        self.gone_after.is_none_or(|n| self.lines.len() < n)
    }

    fn alive(&mut self) -> bool {
        self.gone_after.is_none_or(|n| self.lines.len() < n)
    }
}

/// The harness, yielding once before every store call, so a round-robin
/// executor interleaves two drivers at each of their awaits.
struct Yielding<'a>(&'a Harness);

impl Sql for Yielding<'_> {
    async fn run(&self, s: Stmt) -> Result<Done, slop_ninja_detect_worker::db::DbError> {
        YieldOnce(false).await;
        self.0.run(s).await
    }
    async fn batch(&self, v: Vec<Stmt>) -> Result<Vec<Done>, slop_ninja_detect_worker::db::DbError> {
        YieldOnce(false).await;
        self.0.batch(v).await
    }
}

impl Io for Yielding<'_> {
    fn now_ms(&self) -> u64 {
        self.0.now_ms()
    }
    fn var(&self, name: &str) -> Option<String> {
        self.0.var(name)
    }
    fn fill(&self, buf: &mut [u8]) {
        self.0.fill(buf)
    }
    async fn doc_put(&self, key: &str, bytes: Vec<u8>) -> Result<(), IoError> {
        YieldOnce(false).await;
        self.0.doc_put(key, bytes).await
    }
    async fn doc_get(&self, key: &str) -> Result<Option<Vec<u8>>, IoError> {
        YieldOnce(false).await;
        self.0.doc_get(key).await
    }
    async fn doc_delete(&self, keys: &[String]) -> Result<(), IoError> {
        self.0.doc_delete(keys).await
    }
    async fn doc_list(&self, prefix: &str, cursor: Option<&str>) -> Result<DocPage, IoError> {
        self.0.doc_list(prefix, cursor).await
    }
    async fn owned_add(&self, req: &OwnedAddRequest) -> Result<(), IoError> {
        self.0.owned_add(req).await
    }
    async fn enqueue(&self, msgs: Vec<QueueMessage>) -> Result<(), IoError> {
        self.0.enqueue(msgs).await
    }
    async fn cache_purge(&self, urls: &[String]) -> Result<bool, IoError> {
        self.0.cache_purge(urls).await
    }
    async fn relay_legal(&self, body: Vec<u8>) -> Result<(), IoError> {
        self.0.relay_legal(body).await
    }
    async fn receipt(&self, r: &StoredResult) -> Option<String> {
        self.0.receipt(r).await
    }
    fn metric(&self, point: &'static str, blobs: &[&str]) {
        self.0.metric(point, blobs)
    }
}

/// One flow in a race.
type Racer<'a, T> = Pin<Box<dyn Future<Output = T> + 'a>>;

fn race<T>(futs: Vec<Racer<'_, T>>) -> Vec<T> {
    let mut futs: Vec<Option<Racer<'_, T>>> = futs.into_iter().map(Some).collect();
    let mut out: Vec<Option<T>> = futs.iter().map(|_| None).collect();
    let mut cx = Context::from_waker(Waker::noop());
    for _ in 0..1_000_000 {
        if out.iter().all(Option::is_some) {
            break;
        }
        for (i, f) in futs.iter_mut().enumerate() {
            if let Some(fut) = f
                && let Poll::Ready(v) = fut.as_mut().poll(&mut cx)
            {
                out[i] = Some(v);
                *f = None;
            }
        }
    }
    out.into_iter().map(|o| o.expect("the race settles")).collect()
}

// ------------------------------------------------------------------ helpers

fn quote(
    w: &World,
    l: &FakeLedger,
    text: &str,
    seal: &str,
    p: Option<Principal>,
) -> Result<slop_ninja_detect_page::api::QuoteAnswer, RefusalCode> {
    let req = QuoteRequest { text: text.into(), seal: seal.into() };
    block_on(paid::quote(&w.h, l, &w.cx(), p, &req, &mut |b, _| b <= 8 << 20))
}

fn start(
    w: &World,
    l: &FakeLedger,
    text: &str,
    seal: &str,
    quote_seal: &str,
    p: Option<Principal>,
) -> Result<slop_ninja_detect_page::api::StartAnswer, RefusalCode> {
    let req =
        StartRequest { text: text.into(), seal: seal.into(), quote_seal: quote_seal.into(), expiry: Expiry::Keep };
    block_on(paid::start(&w.h, l, &w.cx(), p, &req, &mut |b, _| b <= 8 << 20))
}

fn drive_with(
    w: &World,
    io: &impl Io,
    l: &FakeLedger,
    s: &FakeScorer<'_>,
    slug: &str,
    calls: &Calls,
) -> (JobEnd, Vec<JobLine>) {
    let open = block_on(paid::open_job(io, w.keys(), slug)).unwrap();
    let d = Drive { io, ledger: l, scorer: s, serving: &w.serving, lease_ms: LEASE, calls };
    let mut sink = Sink::default();
    let end = block_on(d.run(&open, slug, &mut sink));
    (end, sink.lines)
}

fn drive(w: &World, l: &FakeLedger, s: &FakeScorer<'_>, slug: &str) -> (JobEnd, Vec<JobLine>) {
    drive_with(w, &w.h, l, s, slug, &Calls::new(SUBREQUESTS_PER_INVOCATION))
}

/// A started job: its text, slug and manage token.
fn started(w: &World, l: &FakeLedger, words: usize) -> (String, String, String, u32) {
    let text = prose(words);
    let seal = w.free_seal(&text, 1);
    let q = quote(w, l, &text, &seal, Some(reader(false))).unwrap();
    let a = start(w, l, &text, &seal, &q.quote_seal, Some(reader(false))).unwrap();
    (text, a.slug, a.manage, q.sections)
}

fn job_state(w: &World) -> String {
    w.h.db.query_row("SELECT state FROM jobs", [], |r| r.get(0)).unwrap()
}

fn det_state(w: &World) -> String {
    w.h.db.query_row("SELECT state FROM detections", [], |r| r.get(0)).unwrap()
}

fn commit_holds(w: &World) -> Vec<[u8; 32]> {
    w.h.queue
        .borrow()
        .iter()
        .filter_map(|m| match &m.job {
            Job::CommitHold { det_id_h } => Some(det_id_h.0),
            _ => None,
        })
        .collect()
}

// ------------------------------------------------------------------- tests

#[test]
fn a_quote_a_start_and_a_job_make_one_paid_page() {
    let w = World::new();
    let l = FakeLedger::with(100_000);
    let text = prose(3_214);
    let seal = w.free_seal(&text, 1);
    let q = quote(&w, &l, &text, &seal, Some(reader(false))).unwrap();
    let canonical = canon::canon_v1(&text).into_owned();
    let plan = slop_ninja_detect_worker::sections::plan(&canonical, MAX_SECTION_BYTES, &mut |_, _| true).unwrap();
    assert_eq!(q.sections as usize, plan.sections.len());
    assert!(q.sections >= 4);
    assert_eq!(q.words as usize, policy::words(&canonical));
    assert_eq!(q.free_words, plan.sections[0].words);
    assert_eq!(u64::from(q.paid_tokens), plan.paid_tokens);
    assert_eq!((q.draw_plan, q.draw_credit, q.enough), (0, q.paid_tokens, true));
    assert!(q.unscored.is_empty());
    assert_eq!(l.peeks.get(), 1);
    // A quote holds nothing and stores nothing.
    assert_eq!(w.h.count("SELECT count(*) FROM detections"), 0);
    assert!(l.holds.borrow().is_empty());

    let a = start(&w, &l, &text, &seal, &q.quote_seal, Some(reader(false))).unwrap();
    assert_eq!(l.credit.get(), 100_000 - u64::from(q.paid_tokens));
    assert_eq!(det_state(&w), "pending");
    assert_eq!(
        w.h.count("SELECT count(*) FROM detections WHERE kind = 'private_paid' AND owner_via = 'paid' AND seal_nonce_h IS NOT NULL"),
        1
    );
    assert_eq!(w.h.count("SELECT sections_done FROM jobs"), 1, "section 1 is the free check's");
    assert_eq!(w.h.count("SELECT sections_total FROM jobs"), i64::from(q.sections));
    assert_eq!(w.docs_under("j/"), 2, "the job text and section 1");
    let saved = w.h.owned.borrow().clone();
    assert_eq!((saved.len(), saved[0].is_owner, saved[0].summary.kind), (1, true, Kind::PrivatePaid));
    // Nothing derived from the text reaches D1.
    let dump = w.h.dump();
    for word in ["garden", "quiet", "Garden notes"] {
        assert!(!dump.contains(word), "{word}");
    }
    // Pending: the page shows the job's progress.
    let p = w.page(&a.slug).unwrap();
    assert_eq!(p.state, DetState::Pending);
    assert_eq!(p.pending.unwrap().job.unwrap().sections_done, 1);

    let scorer = FakeScorer::default();
    let (end, lines) = drive(&w, &l, &scorer, &a.slug);
    assert_eq!(end, JobEnd::Done, "{lines:?}");
    assert_eq!(scorer.calls.get(), q.sections - 1, "section 1 is not scored again");
    let running: Vec<u32> = lines
        .iter()
        .filter_map(|x| if let JobLine::Running { section, .. } = x { Some(*section) } else { None })
        .collect();
    assert_eq!(running, (2..=q.sections).collect::<Vec<_>>());
    assert_eq!(lines.last(), Some(&JobLine::Done));
    assert_eq!((job_state(&w), det_state(&w)), ("committed".into(), "live".into()));
    assert_eq!(l.commits.get(), 1);
    assert_eq!(w.docs_under("j/"), 0, "the job objects go");
    assert_eq!(w.h.count("SELECT count(*) FROM paid_receipts"), 0, "grants are served, never counted");
    // The page: every section, passages merged, its own contract.
    let p = w.page(&a.slug).unwrap();
    p.check().unwrap();
    let s = p.sections.as_ref().unwrap();
    assert_eq!(s.items.len() as u32, q.sections);
    assert_eq!(s.document.total_sections, q.sections);
    assert_eq!(s.document.strong_sections, (q.sections - 1) / 2);
    assert_eq!(s.items[0].band, Band::Unclear, "section 1 is the free result");
    let run = p.run.as_ref().unwrap();
    assert!(run.band.is_none() && run.length.is_none());
    assert_eq!(run.spans.len() as u32, (q.sections - 1) / 2);
    assert_eq!(p.text.as_ref().unwrap().body.as_deref(), Some(canonical.as_str()));
    assert_eq!(p.receipt.as_deref(), Some("receipt"));
    // A second open shows the page.
    let (end, lines) = drive(&w, &l, &scorer, &a.slug);
    assert_eq!((end, lines), (JobEnd::Shown, vec![JobLine::Done]));
}

#[test]
fn the_quote_refuses_what_cannot_be_checked() {
    let w = World::new();
    let l = FakeLedger::with(100_000);
    let text = prose(2_000);
    let seal = w.free_seal(&text, 1);
    assert_eq!(quote(&w, &l, &text, &seal, None).unwrap_err(), RefusalCode::SignedOut);
    assert_eq!(
        quote(&w, &l, &text, "AQ3vb2xkLXNlYWwtZXhhbXBsZQ", Some(reader(false))).unwrap_err(),
        RefusalCode::SealInvalid
    );
    assert_eq!(
        quote(&w, &l, &prose(2_001), &seal, Some(reader(false))).unwrap_err(),
        RefusalCode::SealInvalid,
        "another text"
    );
    // Line endings and surrounding space do not make another text.
    let respaced = format!("  {}", text.replace('\n', "\r\n"));
    assert!(quote(&w, &l, &respaced, &seal, Some(reader(false))).is_ok());
    // A text the free check scored whole has nothing to buy.
    let short = prose(800);
    let short_seal = w.free_seal(&short, 2);
    assert_eq!(quote(&w, &l, &short, &short_seal, Some(reader(false))).unwrap_err(), RefusalCode::MalformedRequest);
    // Paid checks closed to the public: only a tester gets a quote.
    w.h.vars.borrow_mut().insert("DETECT_PAID".into(), "off".into());
    assert_eq!(quote(&w, &l, &text, &seal, Some(reader(false))).unwrap_err(), RefusalCode::PaidOff);
    assert!(quote(&w, &l, &text, &seal, Some(reader(true))).is_ok());
    w.h.vars.borrow_mut().insert("DETECT_PAID".into(), "on".into());
    // The ledger's refusals pass through.
    l.billing_off.set(true);
    assert_eq!(quote(&w, &l, &text, &seal, Some(reader(false))).unwrap_err(), RefusalCode::PaidOff);
    l.billing_off.set(false);
    // Short of credit: the figures, and no Confirm.
    let poor = FakeLedger::with(10);
    let q = quote(&w, &poor, &text, &seal, Some(reader(false))).unwrap();
    assert!(!q.enough);
    assert_eq!(q.draw_credit, q.paid_tokens);
    // A seal of another package converts no longer.
    let mut moved = World::new();
    moved.serving.load = "b".repeat(64);
    assert_eq!(quote(&moved, &l, &text, &seal, Some(reader(false))).unwrap_err(), RefusalCode::SealInvalid);
}

#[test]
fn a_start_needs_this_account_s_quote_for_this_text() {
    let w = World::new();
    let l = FakeLedger::with(100_000);
    let text = prose(2_500);
    let seal = w.free_seal(&text, 1);
    let q = quote(&w, &l, &text, &seal, Some(reader(false))).unwrap();
    let other = Principal { user_id: UserId([0x11; 16]), ..reader(false) };
    assert_eq!(start(&w, &l, &text, &seal, &q.quote_seal, Some(other)).unwrap_err(), RefusalCode::QuoteInvalid);
    assert_eq!(
        start(&w, &l, &text, &seal, &seal, Some(reader(false))).unwrap_err(),
        RefusalCode::QuoteInvalid,
        "a result seal"
    );
    // Another free seal of the same text: not the one quoted.
    let seal2 = w.free_seal(&text, 2);
    assert_eq!(
        start(&w, &l, &text, &seal2, &q.quote_seal, Some(reader(false))).unwrap_err(),
        RefusalCode::QuoteInvalid
    );
    assert_eq!(
        start(&w, &l, &prose(2_501), &seal, &q.quote_seal, Some(reader(false))).unwrap_err(),
        RefusalCode::SealInvalid
    );
    w.h.vars.borrow_mut().insert("DETECT_STORE".into(), "off".into());
    assert_eq!(start(&w, &l, &text, &seal, &q.quote_seal, Some(reader(false))).unwrap_err(), RefusalCode::StoreOff);
    w.h.vars.borrow_mut().remove("DETECT_STORE");
    assert!(l.holds.borrow().is_empty(), "no refused start holds anything");
    // Not enough credit: 402, nothing written.
    let poor = FakeLedger::with(10);
    assert_eq!(
        start(&w, &poor, &text, &seal, &q.quote_seal, Some(reader(false))).unwrap_err(),
        RefusalCode::NotEnoughCredit
    );
    assert_eq!(w.h.count("SELECT count(*) FROM detections"), 0);
    assert_eq!(w.docs_under("j/"), 0);
    // A confirm sent twice: one hold, one page; the second answers
    // seal_used and gives back nothing the first holds.
    let a = start(&w, &l, &text, &seal, &q.quote_seal, Some(reader(false))).unwrap();
    assert_eq!(start(&w, &l, &text, &seal, &q.quote_seal, Some(reader(false))).unwrap_err(), RefusalCode::SealUsed);
    assert_eq!(l.holds.borrow().len(), 1);
    assert_eq!(l.releases.get(), 0);
    assert_eq!(w.h.count("SELECT count(*) FROM detections"), 1);
    assert_eq!(w.docs_under("j/"), 2);
    // The scan that started a check cannot also make a free link.
    let used = w.h.count("SELECT count(*) FROM detections WHERE seal_nonce_h IS NOT NULL");
    assert_eq!(used, 1);
    let _ = a;
}

#[test]
fn a_quote_whose_hold_was_given_back_starts_nothing() {
    // A start fails after its hold: at the job text's put, or at the insert
    // batch. Nothing is inserted, so the free seal stays unused and the
    // quote seal stays valid, and the hold goes back. The same confirm sent
    // again (back and resubmit, or a replay) must not run on that hold.
    for at_batch in [false, true] {
        let w = World::new();
        let l = FakeLedger::with(100_000);
        let text = prose(2_500);
        let seal = w.free_seal(&text, 1);
        let q = quote(&w, &l, &text, &seal, Some(reader(false))).unwrap();
        if at_batch {
            w.h.before("INSERT INTO jobs", |db| {
                db.execute_batch(
                    "CREATE TEMP TRIGGER no_job BEFORE INSERT ON jobs BEGIN SELECT RAISE(ABORT, 'down'); END;",
                )
                .unwrap();
            });
        } else {
            w.h.fail_doc_put.set(true);
        }
        assert_eq!(
            start(&w, &l, &text, &seal, &q.quote_seal, Some(reader(false))).unwrap_err(),
            RefusalCode::SaveUnavailable,
            "at_batch {at_batch}"
        );
        w.h.fail_doc_put.set(false);
        w.h.exec("DROP TRIGGER IF EXISTS temp.no_job");
        assert_eq!((l.holds.borrow().len(), l.releases.get(), l.commits.get()), (1, 1, 0));
        assert_eq!(l.credit.get(), 100_000, "the hold went back");
        assert_eq!(w.h.count("SELECT count(*) FROM detections"), 0);
        assert_eq!(w.docs_under("j/"), 0);

        // The same quote again: refused, and nothing is placed or written.
        assert_eq!(
            start(&w, &l, &text, &seal, &q.quote_seal, Some(reader(false))).unwrap_err(),
            RefusalCode::QuoteInvalid,
            "at_batch {at_batch}"
        );
        assert_eq!(l.holds.borrow().len(), 1, "no second hold");
        assert_eq!((l.releases.get(), l.commits.get()), (1, 0));
        assert_eq!(l.credit.get(), 100_000);
        assert_eq!(w.h.count("SELECT count(*) FROM detections"), 0);
        assert_eq!(w.h.count("SELECT count(*) FROM jobs"), 0);
        assert_eq!(w.docs_under("j/"), 0);
        assert_eq!(w.h.metric_count("job", "started"), 0);

        // A fresh quote of the same scan starts the check, which commits once.
        let q2 = quote(&w, &l, &text, &seal, Some(reader(false))).unwrap();
        let a = start(&w, &l, &text, &seal, &q2.quote_seal, Some(reader(false))).unwrap();
        let (end, _) = drive(&w, &l, &FakeScorer::default(), &a.slug);
        assert_eq!(end, JobEnd::Done);
        assert_eq!((l.holds.borrow().len(), l.releases.get(), l.commits.get()), (2, 1, 1));
        assert_eq!(l.credit.get(), 100_000 - u64::from(q2.paid_tokens));
        assert_eq!((job_state(&w), det_state(&w)), ("committed".into(), "live".into()));
    }
}

#[test]
fn two_drivers_side_by_side_score_each_section_once() {
    let w = World::new();
    let l = FakeLedger::with(1_000_000);
    let (_, slug, _, k) = started(&w, &l, 6_000);
    let io = Yielding(&w.h);
    let (a, b) = (FakeScorer::default(), FakeScorer::default());
    a.yields.set(true);
    b.yields.set(true);
    let open_a = block_on(paid::open_job(&w.h, w.keys(), &slug)).unwrap();
    let open_b = block_on(paid::open_job(&w.h, w.keys(), &slug)).unwrap();
    let (ba, bb) = (Calls::new(SUBREQUESTS_PER_INVOCATION), Calls::new(SUBREQUESTS_PER_INVOCATION));
    let da = Drive { io: &io, ledger: &l, scorer: &a, serving: &w.serving, lease_ms: LEASE, calls: &ba };
    let db = Drive { io: &io, ledger: &l, scorer: &b, serving: &w.serving, lease_ms: LEASE, calls: &bb };
    let (mut sa, mut sb) = (Sink::default(), Sink::default());
    let fa: Racer<'_, JobEnd> = Box::pin(da.run(&open_a, &slug, &mut sa));
    let fb: Racer<'_, JobEnd> = Box::pin(db.run(&open_b, &slug, &mut sb));
    let ends = race(vec![fa, fb]);
    let done = ends.iter().filter(|e| **e == JobEnd::Done).count();
    let watching = ends.iter().filter(|e| **e == JobEnd::Watching).count();
    assert_eq!((done, watching), (1, 1), "{ends:?}");
    assert_eq!(a.calls.get() + b.calls.get(), k - 1, "every section scored once");
    assert_eq!(l.commits.get(), 1);
    assert_eq!((job_state(&w), det_state(&w)), ("committed".into(), "live".into()));
    assert!(matches!(if ends[0] == JobEnd::Watching { &sa.lines[0] } else { &sb.lines[0] }, JobLine::Progress { .. }));
}

#[test]
fn a_driver_whose_lease_lapsed_loses_to_the_next() {
    let w = World::new();
    let l = FakeLedger::with(1_000_000);
    let (_, slug, _, k) = started(&w, &l, 6_000);
    assert!(k >= 5);
    let winner = FakeScorer::default();
    let winner_end = RefCell::new(None);
    // During the first driver's second section, its lease lapses and a
    // second driver claims the job and finishes it.
    let late = FakeScorer::default();
    *late.hook.borrow_mut() = Some((
        2,
        Box::new(|| {
            w.h.advance(LEASE + 1);
            *winner_end.borrow_mut() = Some(drive(&w, &l, &winner, &slug).0);
        }),
    ));
    let (end, lines) = drive(&w, &l, &late, &slug);
    assert_eq!(end, JobEnd::Lost, "{lines:?}");
    assert_eq!(*winner_end.borrow(), Some(JobEnd::Done));
    assert_eq!(late.calls.get(), 2);
    assert_eq!(winner.calls.get(), k - 2, "the winner resumed after the loser's last fenced section");
    assert_eq!(l.commits.get(), 1);
    assert_eq!((job_state(&w), det_state(&w)), ("committed".into(), "live".into()));
    // The loser's late write of section 3 is an orphan under a finished
    // job; the hourly backstop deletes it once it is an hour old.
    assert_eq!(w.docs_under("j/"), 1);
    w.h.advance(3_600_001);
    block_on(slop_ninja_detect_worker::cron::hourly(&w.h, &w.origins, false)).unwrap();
    assert_eq!(w.docs_under("j/"), 0);
    w.page(&slug).unwrap().check().unwrap();
}

#[test]
fn deleting_the_page_mid_job_cancels_it_and_gives_the_hold_back() {
    let w = World::new();
    let l = FakeLedger::with(1_000_000);
    let (_, slug, manage, _) = started(&w, &l, 6_000);
    let taken = 1_000_000 - l.credit.get();
    let s = FakeScorer::default();
    *s.hook.borrow_mut() = Some((
        2,
        Box::new(|| {
            let req = ManageRequest {
                slug: slug.clone(),
                manage: manage.clone(),
                action: ManageAction::Delete,
                expiry: None,
            };
            block_on(slop_ninja_detect_worker::manage::manage(&w.h, w.keys(), &w.origins, &req)).unwrap();
        }),
    ));
    let (end, _) = drive(&w, &l, &s, &slug);
    assert_eq!(end, JobEnd::Lost);
    assert_eq!((job_state(&w), det_state(&w)), ("cancelled".into(), "deleted".into()));
    // The loser deleted what it wrote after the cancel; the hold goes back
    // through commit_hold, which also removes the job objects.
    let ids = commit_holds(&w);
    assert_eq!(ids.len(), 1);
    block_on(paid::end_hold(&w.h, &l, &ids[0])).unwrap();
    assert_eq!((l.releases.get(), l.commits.get()), (1, 0));
    assert_eq!(l.credit.get(), 1_000_000, "{taken} tokens came back");
    assert_eq!(w.docs_under("j/") + w.docs_under("p/"), 0);
    assert_eq!(w.h.count("SELECT count(*) FROM jobs WHERE hold_ended_at IS NOT NULL"), 1);
    // A repeat of the message changes nothing.
    block_on(paid::end_hold(&w.h, &l, &ids[0])).unwrap();
    assert_eq!(l.releases.get(), 1);
    assert_eq!(w.page(&slug).unwrap().state, DetState::Deleted);
}

#[test]
fn a_section_refused_for_good_fails_the_page_and_charges_nothing() {
    let w = World::new();
    let l = FakeLedger::with(1_000_000);
    let (_, slug, _, _) = started(&w, &l, 4_000);
    let s = FakeScorer::default();
    s.refuse_on.set(Some((2, ScoreFail::Refused(RefusalCode::Scorer))));
    let (end, lines) = drive(&w, &l, &s, &slug);
    assert_eq!(end, JobEnd::Failed(RefusalCode::Scorer));
    assert_eq!(lines.last(), Some(&JobLine::Failed { code: RefusalCode::Scorer }));
    assert_eq!((job_state(&w), det_state(&w)), ("failed".into(), "failed".into()));
    assert_eq!((l.releases.get(), l.commits.get()), (1, 0));
    assert_eq!(l.credit.get(), 1_000_000);
    assert_eq!(w.docs_under("j/"), 0);
    let p = w.page(&slug).unwrap();
    assert_eq!(p.failure.unwrap().code, RefusalCode::Scorer);
    p.check().unwrap();
    // A failed job is never driven again.
    let (end, lines) = drive(&w, &l, &FakeScorer::default(), &slug);
    assert_eq!((end, lines), (JobEnd::Shown, vec![JobLine::Failed { code: RefusalCode::Scorer }]));
}

#[test]
fn a_transient_stop_resumes_where_it_stopped() {
    let w = World::new();
    let l = FakeLedger::with(1_000_000);
    let (_, slug, _, k) = started(&w, &l, 6_000);
    let s = FakeScorer::default();
    s.refuse_on.set(Some((3, ScoreFail::Transient)));
    let (end, lines) = drive(&w, &l, &s, &slug);
    assert_eq!(end, JobEnd::Stopped);
    assert_eq!(lines.last(), Some(&JobLine::Progress { section: 3, of: k }));
    assert_eq!(w.h.count("SELECT sections_done FROM jobs"), 3);
    assert_eq!(w.h.count("SELECT count(*) FROM jobs WHERE lease_token IS NULL"), 1, "the lease is let go");
    // The next open claims at once and finishes.
    let (end, _) = drive(&w, &l, &FakeScorer::default(), &slug);
    assert_eq!(end, JobEnd::Done);
    assert_eq!(l.commits.get(), 1);
    // A reader who leaves stops the driver too.
    let w2 = World::new();
    let l2 = FakeLedger::with(1_000_000);
    let (_, slug2, _, _) = started(&w2, &l2, 6_000);
    let open = block_on(paid::open_job(&w2.h, w2.keys(), &slug2)).unwrap();
    let b = Calls::new(SUBREQUESTS_PER_INVOCATION);
    let s2 = FakeScorer::default();
    let d = Drive { io: &w2.h, ledger: &l2, scorer: &s2, serving: &w2.serving, lease_ms: LEASE, calls: &b };
    let mut sink = Sink { gone_after: Some(2), ..Sink::default() };
    assert_eq!(block_on(d.run(&open, &slug2, &mut sink)), JobEnd::Stopped);
    assert_eq!(s2.calls.get(), 2);
}

#[test]
fn a_failed_commit_leaves_the_result_shown_and_retries() {
    let w = World::new();
    let l = FakeLedger::with(1_000_000);
    l.cash.set(true);
    let (_, slug, _, _) = started(&w, &l, 3_000);
    l.down.set(true);
    let (end, _) = drive(&w, &l, &FakeScorer::default(), &slug);
    assert_eq!(end, JobEnd::Done);
    assert_eq!((job_state(&w), det_state(&w)), ("scored".into(), "live".into()));
    w.page(&slug).unwrap().check().unwrap();
    let ids = commit_holds(&w);
    assert_eq!(ids.len(), 1);
    assert!(block_on(paid::end_hold(&w.h, &l, &ids[0])).is_err(), "still down: retried");
    l.down.set(false);
    block_on(paid::end_hold(&w.h, &l, &ids[0])).unwrap();
    assert_eq!(job_state(&w), "committed");
    assert_eq!(l.commits.get(), 1);
    // Cash-backed tokens make one pot receipt, once.
    assert_eq!(w.h.count("SELECT count(*) FROM paid_receipts WHERE cash = 1 AND channel = 'paid_page'"), 1);
    block_on(paid::end_hold(&w.h, &l, &ids[0])).unwrap();
    assert_eq!(w.h.count("SELECT count(*) FROM paid_receipts"), 1);
    // The sweep asks again for a hold whose end is overdue, and only then.
    let w3 = World::new();
    let l3 = FakeLedger::with(1_000_000);
    let (_, slug3, _, _) = started(&w3, &l3, 3_000);
    l3.down.set(true);
    drive(&w3, &l3, &FakeScorer::default(), &slug3);
    w3.h.queue.borrow_mut().clear();
    assert_eq!(block_on(paid::sweep(&w3.h)).unwrap(), 0);
    w3.h.advance(3_600_001);
    assert_eq!(block_on(paid::sweep(&w3.h)).unwrap(), 1);
}

#[test]
fn closed_paid_checks_pause_public_jobs_and_run_testers() {
    let w = World::new();
    let l = FakeLedger::with(1_000_000);
    let (_, slug, _, k) = started(&w, &l, 3_000);
    w.h.vars.borrow_mut().insert("DETECT_PAID".into(), "off".into());
    let (end, lines) = drive(&w, &l, &FakeScorer::default(), &slug);
    assert_eq!((end, lines), (JobEnd::Stopped, vec![JobLine::Progress { section: 1, of: k }]));
    assert_eq!(w.h.count("SELECT count(*) FROM jobs WHERE lease_token IS NULL"), 1);
    // A tester's job runs while the switch is off.
    let wt = World::new();
    wt.h.vars.borrow_mut().insert("DETECT_PAID".into(), "off".into());
    let lt = FakeLedger::with(1_000_000);
    let text = prose(3_000);
    let seal = wt.free_seal(&text, 1);
    let q = quote(&wt, &lt, &text, &seal, Some(reader(true))).unwrap();
    let a = start(&wt, &lt, &text, &seal, &q.quote_seal, Some(reader(true))).unwrap();
    assert_eq!(drive(&wt, &lt, &FakeScorer::default(), &a.slug).0, JobEnd::Done);
}

#[test]
fn an_expired_page_s_running_job_is_cancelled_by_the_sweep() {
    let w = World::new();
    let l = FakeLedger::with(1_000_000);
    let text = prose(3_000);
    let seal = w.free_seal(&text, 1);
    let q = quote(&w, &l, &text, &seal, Some(reader(false))).unwrap();
    let req = StartRequest { text: text.clone(), seal: seal.clone(), quote_seal: q.quote_seal, expiry: Expiry::Day };
    let a = block_on(paid::start(&w.h, &l, &w.cx(), Some(reader(false)), &req, &mut |_, _| true)).unwrap();
    w.h.advance(86_400_001);
    // The job route refuses an expired page before any stream.
    assert_eq!(block_on(paid::open_job(&w.h, w.keys(), &a.slug)).unwrap_err(), RefusalCode::PageGone);
    assert_eq!(block_on(paid::sweep(&w.h)).unwrap(), 1);
    assert_eq!(job_state(&w), "cancelled");
    let ids = commit_holds(&w);
    block_on(paid::end_hold(&w.h, &l, &ids[0])).unwrap();
    assert_eq!(l.credit.get(), 1_000_000);
    assert_eq!(w.docs_under("j/"), 0);
}

#[test]
fn an_account_deletion_cancels_its_running_job() {
    let w = World::new();
    let l = FakeLedger::with(1_000_000);
    let (_, _slug, _, _) = started(&w, &l, 3_000);
    let msg = QueueMessage::new(Job::OwnerPurge {
        user_id: reader(false).user_id,
        mode: slop_ninja_detect_page::codes::PurgeMode::Delete,
    });
    assert_eq!(
        block_on(slop_ninja_detect_worker::queue::consume(&w.h, &w.store, &msg)),
        slop_ninja_detect_worker::queue::Ack::Done
    );
    assert_eq!((job_state(&w), det_state(&w)), ("cancelled".into(), "deleted".into()));
    let ids = commit_holds(&w);
    assert_eq!(ids.len(), 1);
    block_on(paid::end_hold(&w.h, &l, &ids[0])).unwrap();
    assert_eq!(l.releases.get(), 1);
}

/// The subrequest calls of a long job, measured on the driver's own count
/// (every D1 statement or batch, R2 call, ledger call and the scorer's
/// gate and lane calls), and a calls too small for the whole job stopping
/// cleanly and resuming.
#[test]
fn the_subrequest_calls_of_a_long_job() {
    let w = World::new();
    let l = FakeLedger::with(10_000_000);
    let (text, slug, _, k) = started(&w, &l, 100_000);
    assert!(text.len() < PAID_TEXT_BYTES);
    let b = Calls::new(SUBREQUESTS_PER_INVOCATION);
    let (end, _) = drive_with(&w, &w.h, &l, &FakeScorer::default(), &slug, &b);
    assert_eq!(end, JobEnd::Done);
    let used = b.used();
    eprintln!(
        "MEASURED: {k} sections ({} words, {} bytes): {used} subrequests in one invocation (per section {:.2}; limit {})",
        100_000,
        text.len(),
        f64::from(used) / f64::from(k),
        SUBREQUESTS_PER_INVOCATION
    );
    assert!(used <= SUBREQUESTS_PER_INVOCATION);
    // A calls for half the job: the driver keeps room for one more section
    // and the whole end, stops cleanly, lets its lease go, and the next open
    // resumes.
    let w2 = World::new();
    let l2 = FakeLedger::with(10_000_000);
    let (_, slug2, _, k2) = started(&w2, &l2, 100_000);
    let small = used / 2;
    assert!(small > paid::assembly_subrequests(k2) + paid::SECTION_SUBREQUESTS);
    let mut opens = 0;
    loop {
        opens += 1;
        assert!(opens < 20);
        let b = Calls::new(small);
        let (end, _) = drive_with(&w2, &w2.h, &l2, &FakeScorer::default(), &slug2, &b);
        assert!(b.used() <= small, "{} > {small}", b.used());
        if end == JobEnd::Done {
            break;
        }
        assert_eq!(end, JobEnd::Stopped);
    }
    eprintln!("MEASURED: with a calls of {small}, {k2} sections took {opens} opens");
    assert!(opens >= 3);
    assert_eq!(l2.commits.get(), 1);
}

#[test]
fn the_paid_page_s_figures_follow_its_sections() {
    let w = World::new();
    let l = FakeLedger::with(1_000_000);
    let (text, slug, _, k) = started(&w, &l, 5_000);
    drive(&w, &l, &FakeScorer::default(), &slug);
    let p = w.page(&slug).unwrap();
    let s = p.sections.as_ref().unwrap();
    // Owned words partition the document; the share is owned-word weighted.
    let mut next = 1;
    let (mut weighted, mut total) = (0u64, 0u64);
    for it in &s.items {
        assert_eq!(it.owned_from_word, next);
        next = it.owned_to_word + 1;
        let n = u64::from(it.owned_to_word + 1 - it.owned_from_word);
        weighted += n * u64::from(it.share_pct);
        total += n;
    }
    assert_eq!(next - 1, policy::words(&canon::canon_v1(&text)) as u32);
    assert_eq!(u64::from(s.document.share_pct), weighted / total);
    assert_eq!(s.items.len() as u32, k);
    // Passages lie on the text's words, at least 24 each.
    let body = p.text.as_ref().unwrap().body.as_deref().unwrap();
    let u16: Vec<u16> = body.encode_utf16().collect();
    for sp in &p.run.as_ref().unwrap().spans {
        let passage = String::from_utf16(&u16[sp.start as usize..sp.end as usize]).unwrap();
        assert_eq!(policy::words(&passage) as u32, sp.words);
        assert!(sp.words >= 24);
    }
}
