//! Measures the relay's real heap peak under a counting allocator for its
//! largest inputs, read the way `cf.rs` reads them (through a
//! [`Reader`] within the input's bound), and checks that the accounting
//! covers it: the budget's accounted peak plus
//! [`arena::UNACCOUNTED_ALLOWANCE`] (a quarter of
//! [`arena::RESERVED_BYTES`]) is at least the measured peak.
//!
//! - A post at its bound: many of the smallest events, one event whose
//!   detail is all multi-byte characters (the largest quoted-printable
//!   expansion), and one with the largest subject.
//! - A tick while the publisher is down, with a 1 MiB coordinator status
//!   body (its route scope) for each Worker.
//!
//! What the budget does not account (the decoded post and its claimed
//! copies, the decoded status view, the ledger's rows, which live in the
//! Durable Object's SQLite outside Wasm memory in the Worker) is what the
//! allowance covers.

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::RefCell;
use std::sync::atomic::{AtomicUsize, Ordering};

use slop_ninja_alerts_relay::arena::{self, Budget, Reader};
use slop_ninja_alerts_relay::block_on;
use slop_ninja_alerts_relay::config;
use slop_ninja_alerts_relay::ledger::{MemTable, StoreError};
use slop_ninja_alerts_relay::mime::Message;
use slop_ninja_alerts_relay::relay::{self, Mailer, SendError, TickHooks, TickReads, TickState};
use slop_ninja_alerts_relay::rules::{HeartbeatRecord, Intake};
use slop_ninja_alerts_relay::{status, tracker};
use slop_ninja_alerts_wire::{
    Event, EventKind, MAX_STRING_BYTES, POST_EVENTS_BYTES, POST_SCHEMA, Post, post_bound,
};

struct Counting;

static LIVE: AtomicUsize = AtomicUsize::new(0);
static PEAK: AtomicUsize = AtomicUsize::new(0);

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let p = unsafe { System.alloc(layout) };
        if !p.is_null() {
            let live = LIVE.fetch_add(layout.size(), Ordering::Relaxed) + layout.size();
            PEAK.fetch_max(live, Ordering::Relaxed);
        }
        p
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) };
        LIVE.fetch_sub(layout.size(), Ordering::Relaxed);
    }
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        // Counted as a fresh allocation while the old one is still held (a
        // moving realloc copies before it frees).
        let p = unsafe { System.realloc(ptr, layout, new_size) };
        if !p.is_null() {
            let live = LIVE.fetch_add(new_size, Ordering::Relaxed) + new_size;
            PEAK.fetch_max(live, Ordering::Relaxed);
            LIVE.fetch_sub(layout.size(), Ordering::Relaxed);
        }
        p
    }
}

#[global_allocator]
static GLOBAL: Counting = Counting;

const RULES: &str = "slop_ninja.publisher.alerts/v3";
const T0: u64 = 1_793_627_107_000;

/// Accepts every message and keeps none of it.
struct Discard;

impl Mailer for Discard {
    async fn send(&self, _m: &Message) -> Result<(), SendError> {
        Ok(())
    }
}

/// A lease that is always held and a state row kept as its encoded size
/// (the Worker writes it to SQLite).
#[derive(Default)]
struct Hooks(RefCell<usize>);

impl TickHooks for Hooks {
    fn renew(&self, _ms: u64) -> bool {
        true
    }
    fn save(&self, st: &TickState) -> Result<(), StoreError> {
        *self.0.borrow_mut() = serde_json::to_vec(st).map_or(0, |v| v.len());
        Ok(())
    }
}

fn cfg() -> config::Config {
    let vars = [
        ("NETWORK", "testnet"),
        ("ALERT_EMAIL_ENABLED", "true"),
        ("ALERT_EMAIL_FROM", "alerts@example.invalid"),
        ("ALERT_EMAIL_TO", "ops@example.invalid"),
        // Every rule these measurements deliver, with a one-second interval,
        // so each event is emailed and the largest messages are built.
        (
            "ALERT_EMAIL_RULES",
            "r,publication_refused,publication_pending,publisher_silent,coordinator_unwatched,spend_tracker_stale",
        ),
        ("ALERT_EMAIL_MIN_INTERVAL_SECS", "1"),
    ];
    config::parse(|k| {
        vars.iter()
            .find(|(n, _)| *n == k)
            .map(|(_, v)| (*v).to_owned())
    })
    .unwrap()
}

/// Reads `body` as `cf.rs` does: the reservation, then the chunks.
fn read<'a>(budget: &'a Budget, body: &[u8], bound: u64) -> (Vec<u8>, arena::Hold<'a>) {
    let mut r = Reader::new(budget, bound, Some(body.len() as u64)).unwrap();
    for chunk in body.chunks(4096) {
        r.push(chunk).unwrap();
    }
    r.finish()
}

fn start() -> usize {
    let before = LIVE.load(Ordering::Relaxed);
    PEAK.store(before, Ordering::Relaxed);
    before
}

fn measure_post(events: Vec<Event>) -> (usize, u64) {
    let body = Post {
        schema: POST_SCHEMA.into(),
        rules: RULES.into(),
        network: "testnet".into(),
        events,
    }
    .body();
    assert!(
        body.len() <= post_bound(),
        "{} > {}",
        body.len(),
        post_bound()
    );
    let c = cfg();
    let budget = Budget::new(arena::READBACK_BUDGET);
    let before = start();
    let table = MemTable::default();
    let (bytes, hold) = read(&budget, &body, post_bound() as u64);
    let a = block_on(relay::handle_post(
        &bytes, &c, &table, &Discard, &budget, T0,
    ));
    assert_eq!(a.status, 200, "{}", a.body);
    // Every event went into the message (none deferred by the policy).
    assert!(a.body.ends_with(r#""skipped":0}"#), "{}", a.body);
    drop(hold);
    drop(bytes);
    let peak = PEAK.load(Ordering::Relaxed) - before;
    drop(table);
    (peak, budget.peak())
}

fn measure_tick() -> (usize, u64) {
    let pad = "a".repeat(arena::STATUS_BODY_BOUND as usize - 200);
    let status_body = format!(
        r#"{{"service":"x","chain_journal":{{"journal":{{"confirmed_age_ms":999999999,"fault":{{"kind":"gap"}}}},"signing":{{"state":"fault"}}}},"pad":"{pad}"}}"#
    );
    assert!(status_body.len() as u64 <= arena::STATUS_BODY_BOUND);
    let c = cfg();
    let budget = Budget::new(arena::READBACK_BUDGET);
    let hooks = Hooks::default();
    let before = start();
    let table = MemTable::default();
    // Each status body is read and decoded on its own, as `read_status`
    // does; only the decoded view is kept.
    let mut views = Vec::new();
    for _ in 0..2 {
        let (bytes, hold) = read(&budget, status_body.as_bytes(), arena::STATUS_BODY_BOUND);
        views.push(status::parse(&bytes).map_or(status::Read::Unanswered, status::Read::Ok));
        drop(hold);
    }
    let workers: [status::Read; 2] = [views.remove(0), views.remove(0)];
    let intake = Intake::default();
    let tr = tracker::Read::Missing;
    let hb: Option<&HeartbeatRecord> = None;
    let reads = TickReads {
        heartbeat: hb,
        intake: &intake,
        tracker: &tr,
        workers: Some(&workers),
    };
    let mut st = TickState {
        watch_since_ms: Some(T0 - 3_600_000),
        ..TickState::default()
    };
    let r = block_on(relay::tick(
        &mut st, &c, &reads, &table, &Discard, &budget, &hooks, T0,
    ))
    .unwrap();
    assert!(r.sent >= 3, "{r:?}");
    let peak = PEAK.load(Ordering::Relaxed) - before;
    drop(table);
    (peak, budget.peak())
}

/// The most events a post holds that the email policy emails: the shortest
/// listed rule, and one subject each (the shortest distinct ones), so no
/// opening is deferred by the per-subject interval.
fn smallest_events() -> Vec<Event> {
    const DIGITS: &[u8] = b"0123456789abcdefghijklmnopqrstuvwxyz";
    let subject = |mut n: usize| {
        let mut s = Vec::new();
        loop {
            s.push(DIGITS[n % DIGITS.len()]);
            n /= DIGITS.len();
            if n == 0 {
                break;
            }
        }
        String::from_utf8(s).unwrap()
    };
    let mut out = Vec::new();
    let mut size = 0;
    let mut since = 0;
    loop {
        let e = Event::new(
            RULES,
            EventKind::Firing,
            "r",
            &subject(since as usize),
            "",
            since,
            None,
        );
        if size + e.wire_len() + 1 > POST_EVENTS_BYTES {
            break;
        }
        size += e.wire_len() + 1;
        since += 1;
        out.push(e);
    }
    out
}

fn widest_detail() -> Vec<Event> {
    let mut e = Event::new(
        RULES,
        EventKind::Reminder,
        "publication_refused",
        "testnet-c9",
        &"\u{e9}".repeat(POST_EVENTS_BYTES),
        5,
        Some(6),
    );
    assert!(e.fit(POST_EVENTS_BYTES - 1));
    vec![e]
}

fn longest_subject() -> Vec<Event> {
    let mut e = Event::new(
        RULES,
        EventKind::Firing,
        "publication_pending",
        &"s".repeat(MAX_STRING_BYTES),
        &"=".repeat(POST_EVENTS_BYTES),
        5,
        None,
    );
    assert!(e.fit(POST_EVENTS_BYTES - 1));
    vec![e]
}

#[test]
fn accounting_covers_the_measured_peak() {
    // One test, so the global counters see one measurement at a time.
    let allowance = arena::UNACCOUNTED_ALLOWANCE as usize;
    for (name, events) in [
        ("smallest events", smallest_events()),
        ("widest detail", widest_detail()),
        ("longest subject", longest_subject()),
    ] {
        let n = events.len();
        let (real, accounted) = measure_post(events);
        eprintln!(
            "post, {name} ({n} events): measured heap peak {real} B, accounted {accounted} B"
        );
        assert!(
            real <= accounted as usize + allowance,
            "{name}: measured {real} exceeds accounted {accounted} plus the allowance {allowance}"
        );
        assert!(accounted <= arena::post_footprint());
    }
    let (real, accounted) = measure_tick();
    eprintln!(
        "tick with two 1 MiB status bodies: measured heap peak {real} B, accounted {accounted} B"
    );
    assert!(
        real <= accounted as usize + allowance,
        "tick: measured {real} exceeds accounted {accounted} plus the allowance {allowance}"
    );
    assert!(accounted <= arena::tick_footprint());
    // That the working set fits the linked memory with the reserve left
    // whole is a compile-time assertion in `arena`.
}
