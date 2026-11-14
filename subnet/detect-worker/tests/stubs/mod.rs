//! Stubs of the URL pipeline's services for the native tests: the fetch
//! Worker (pages and X's embed), `DetectHost`, the scorer and IndexNow.
//! Every stub counts its calls. Post ids, texts and hosts are synthetic.

#![allow(dead_code)]

use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, BTreeSet};

use slop_ninja_detect_fetch_wire::{
    Adapter, FetchAnswer, FetchRequest, Meta, Outcome, X_CHECK_K, XCheckAnswer, XCheckRequest, XControl, XExists,
    XPostAnswer, XPostRequest,
};
use slop_ninja_detect_page::codes::{Band, CutKind, LengthBin, Reputation, ShortResult};
use slop_ninja_detect_page::page::Span;
use slop_ninja_detect_page::refusal::{ApiRefusal, RefusalCode};
use slop_ninja_detect_worker::db::IoError;
use slop_ninja_detect_worker::doc::{Serving, StoredResult};
use slop_ninja_detect_worker::net::{FetchApi, HostApi, Pinger, Screen, Slot};
use slop_ninja_detect_worker::url_flow::{Scored, UrlScorer};
use slop_ninja_detect_worker::xpost::X_EPOCH_MS;

/// An AI-like sentence the stub scorer highlights (31 words).
pub const AI1: &str = "In today's fast-paced world, community gardens serve as vibrant hubs that foster connection, \
promote sustainability, and empower residents to take meaningful ownership of their shared green spaces.";
/// Another (28 words).
pub const AI2: &str = "Moreover, by leveraging innovative composting strategies and data-driven planting schedules, \
organizers can unlock unprecedented yields while ensuring that every volunteer feels seen and inspired.";
/// A human-like sentence (15 words).
pub const HUMAN: &str = "We planted the tomatoes late this year because the frost hung around until the second week.";

/// A page of `n` sentences with the two AI sentences at 3 and 9.
pub fn article(n: usize) -> String {
    let mut v = Vec::new();
    for i in 0..n {
        v.push(match i {
            3 => AI1,
            9 => AI2,
            _ => HUMAN,
        });
    }
    v.join(" ")
}

/// An X post id created `age_ms` before `now`.
pub fn post_id(now: u64, age_ms: u64, n: u64) -> u64 {
    ((now - age_ms - X_EPOCH_MS) << 22) | n
}

pub fn serving() -> Serving {
    Serving {
        load: "load-d24".into(),
        label: "span package d24".into(),
        published: "e2536d12dff593d7b11895b3cbf6b10232601abfbebde248325afd707722b5e8".into(),
        pending: false,
        scorer_build: "7030a86f".into(),
        evidence: "evidence-d24-web-v1".into(),
    }
}

// ----------------------------------------------------------------- fetch

/// The fetch Worker: pages by URL, posts by id, existence answers by id.
#[derive(Default)]
pub struct FetchStub {
    pub pages: RefCell<BTreeMap<String, FetchAnswer>>,
    pub posts: RefCell<BTreeMap<u64, XPostAnswer>>,
    /// Existence answers; an id not listed answers `present`.
    pub exists: RefCell<BTreeMap<u64, XExists>>,
    pub controls_fail: Cell<Option<XControl>>,
    pub fail_calls: Cell<u32>,
    pub fetch_calls: Cell<u32>,
    pub post_calls: Cell<u32>,
    pub check_calls: Cell<u32>,
    /// Every id asked by the existence check, in order.
    pub asked: RefCell<Vec<u64>>,
    /// The most requests one round held at once.
    pub max_in_flight: Cell<usize>,
}

impl FetchStub {
    pub fn page(&self, url: &str, text: &str) {
        self.pages.borrow_mut().insert(url.to_owned(), ok_page(url, text));
    }
}

pub fn ok_page(url: &str, text: &str) -> FetchAnswer {
    FetchAnswer {
        v: "slop_ninja_detect_fetch_v1".into(),
        outcome: Outcome::Ok,
        adapter: Adapter::Web,
        final_url: Some(url.to_owned()),
        canonical: None,
        meta: Some(Meta { title: Some("Garden notes".into()), site_name: Some("Example".into()), ..Meta::default() }),
        text: Some(text.to_owned()),
        etag: None,
        last_modified: None,
        fresh_ms: None,
        retry_after_ms: None,
        hops: 0,
        body_bytes: text.len() as u64,
        stripped_bytes: text.len() as u64,
        extract_v: "dom_smoothie-0.17".into(),
    }
}

pub fn failed_page(outcome: Outcome) -> FetchAnswer {
    FetchAnswer { outcome, text: None, meta: None, final_url: None, ..ok_page("https://example.com/", "") }
}

impl FetchApi for FetchStub {
    async fn fetch(&self, req: &FetchRequest) -> Result<FetchAnswer, IoError> {
        self.fetch_calls.set(self.fetch_calls.get() + 1);
        if self.fail_calls.get() > 0 {
            self.fail_calls.set(self.fail_calls.get() - 1);
            return Err(IoError);
        }
        Ok(self.pages.borrow().get(&req.url).cloned().unwrap_or_else(|| failed_page(Outcome::NotFound)))
    }

    async fn x_post(&self, req: &XPostRequest) -> Result<XPostAnswer, IoError> {
        self.post_calls.set(self.post_calls.get() + 1);
        Ok(self.posts.borrow().get(&req.id).cloned().unwrap_or(XPostAnswer::Unavailable))
    }

    async fn x_check(&self, req: &XCheckRequest) -> Result<XCheckAnswer, IoError> {
        self.check_calls.set(self.check_calls.get() + 1);
        if let Some(c) = self.controls_fail.get() {
            return Ok(XCheckAnswer::ControlsFailed { control: c });
        }
        // Rounds of at most k requests in flight; after an `unexpected` or
        // `failed` answer, the requests in flight finish and no other starts.
        let mut answers = Vec::with_capacity(req.ids.len());
        let mut stop = false;
        for round in req.ids.chunks(X_CHECK_K) {
            if stop {
                answers.extend(round.iter().map(|_| XExists::NotChecked));
                continue;
            }
            self.max_in_flight.set(self.max_in_flight.get().max(round.len()));
            for id in round {
                self.asked.borrow_mut().push(*id);
                let a = self.exists.borrow().get(id).copied().unwrap_or(XExists::Present);
                stop |= matches!(a, XExists::Unexpected | XExists::Failed { .. });
                answers.push(a);
            }
        }
        Ok(XCheckAnswer::Checked { answers })
    }
}

// ------------------------------------------------------------------ host

#[derive(Default)]
pub struct HostStub {
    pub risky: RefCell<BTreeSet<String>>,
    pub disallowed: RefCell<BTreeSet<String>>,
    pub ranked: RefCell<BTreeSet<String>>,
    pub adult: RefCell<BTreeSet<String>>,
    pub screen_calls: Cell<u32>,
    pub slot_calls: Cell<u32>,
    pub released: Cell<u32>,
}

impl HostApi for HostStub {
    async fn screen(&self, _registrable: &str, host: &str) -> Result<Screen, IoError> {
        self.screen_calls.set(self.screen_calls.get() + 1);
        Ok(if self.risky.borrow().contains(host) { Screen::SecurityRisk } else { Screen::Clear })
    }

    async fn slot(&self, _registrable: &str, url: &str, _lease_ms: u64) -> Result<Slot, IoError> {
        self.slot_calls.set(self.slot_calls.get() + 1);
        Ok(if self.disallowed.borrow().contains(url) { Slot::Disallowed } else { Slot::Granted { slot: "s".into() } })
    }

    async fn release(&self, _registrable: &str, _slot: &str, _retry_after_ms: Option<u64>) {
        self.released.set(self.released.get() + 1);
    }

    async fn reputation(&self, registrable: &str) -> Result<(Reputation, String), IoError> {
        let r = if self.adult.borrow().contains(registrable) {
            Reputation::Adult
        } else if self.ranked.borrow().contains(registrable) {
            Reputation::Ranked
        } else {
            Reputation::Unranked
        };
        Ok((r, "2026-11-09".into()))
    }

    async fn wait_until(&self, _t: u64) {}
}

// ---------------------------------------------------------------- scorer

/// Scores by markers: under 24 words `too_short`; `[fr]` `not_english`;
/// under 150 words a short result (strong when an AI sentence is present);
/// otherwise band strong with each AI sentence a passage.
#[derive(Default)]
pub struct ScorerStub {
    pub calls: Cell<u32>,
    /// Every text it saw (the tests check no X text is kept elsewhere).
    pub seen: RefCell<Vec<String>>,
}

fn spans(text: &str, conf: bool) -> Vec<Span> {
    let mut v = Vec::new();
    for (needle, c) in [(AI1, 91u8), (AI2, 97u8)] {
        if let Some(at) = text.find(needle) {
            let start = text[..at].encode_utf16().count() as u32;
            v.push(Span {
                start,
                end: start + needle.encode_utf16().count() as u32,
                words: needle.split_whitespace().count() as u32,
                confidence: conf.then_some(c),
            });
        }
    }
    v.sort_by_key(|s| s.start);
    v
}

impl UrlScorer for ScorerStub {
    async fn score(&self, canonical: &str) -> Result<Scored, ApiRefusal> {
        self.calls.set(self.calls.get() + 1);
        self.seen.borrow_mut().push(canonical.to_owned());
        let words = canonical.split_whitespace().count() as u32;
        if words < 24 {
            return Err(ApiRefusal::new(RefusalCode::TooShort));
        }
        if canonical.contains("[fr]") {
            return Err(ApiRefusal::new(RefusalCode::NotEnglish));
        }
        let s = serving();
        let short = words < 150;
        let sp = spans(canonical, !short);
        let strong = !sp.is_empty();
        let result = StoredResult {
            scored_at: 1_795_169_040_000,
            package_load: s.load.clone(),
            package_label: s.label.clone(),
            package_pub: s.published.clone(),
            publication_pending: false,
            scorer_build: s.scorer_build.clone(),
            policy: "long-v1".into(),
            evidence: s.evidence.clone(),
            band: (!short).then_some(if strong { Band::Strong } else { Band::Weak }),
            short: short.then_some(if strong { ShortResult::Strong } else { ShortResult::None }),
            confidence_tenths: (!short).then_some(if strong { 973 } else { 312 }),
            confidence_text: (!short).then(|| if strong { "97.3%".into() } else { "31.2%".into() }),
            share_pct: (!short).then_some(if strong { 43 } else { 2 }),
            share_text: (!short).then(|| if strong { "about 43%".into() } else { "about 2%".into() }),
            words,
            total_words: words,
            cut: CutKind::None,
            tokens: words * 2,
            length: LengthBin::of_words(words),
            spans: if strong { sp } else { Vec::new() },
            spans_note: None,
            markers: Vec::new(),
            invisible: false,
            raw: None,
        };
        Ok(Scored { result, scored: canonical.to_owned() })
    }
}

#[derive(Default)]
pub struct PingStub {
    pub bodies: RefCell<Vec<Vec<u8>>>,
}

impl Pinger for PingStub {
    async fn indexnow(&self, body: Vec<u8>) -> Result<(), IoError> {
        self.bodies.borrow_mut().push(body);
        Ok(())
    }
}
