//! The billing ledger on the account store's own migration: operator grants
//! and plans, holds, their one end (commit or release), an ended hold's
//! `idem` refused, the `BILLING` switch, the quote quota, and the races that
//! matter (holds raced against a balance, one `idem` raced, a commit raced
//! against a release).
//!
//! The race tests drive the ledger's own futures through a host that yields
//! before every store call and a round-robin executor, so every interleaving
//! point a Worker has between its read and its batch is exercised; each
//! batch runs as one transaction, as D1 runs it.

mod support;

use std::cell::Cell;
use std::future::Future;
use std::pin::Pin;
use std::task::{Context, Poll, Waker};

use serde_json::{Value, json};
use slop_ninja_account_wire::{HoldAnswer, HoldEnd, HoldRequest};
use slop_ninja_account_worker::host::{
    DbError, Host, NetError, OutReply, OutRequest, QueueUnavailable, QuotaTake, QuotaUnavailable, Row, Stmt,
};
use slop_ninja_account_worker::ledger::{self, End};
use slop_ninja_account_worker::store::Switches;
use slop_ninja_auth_core::mail::{Message, SendFailure};
use slop_ninja_auth_core::policy::TokenBucket;
use slop_ninja_detect_page::ae::Point;
use slop_ninja_detect_page::queue::QueueMessage;
use slop_ninja_detect_page::{Channel, RefusalCode, UserId};
use support::*;

/// The span package's published digest (public).
const PKG: &str = "e2536d12dff593d7b11895b3cbf6b10232601abfbebde248325afd707722b5e8";

fn grants() -> Result<slop_ninja_account_worker::Config, slop_ninja_account_worker::ConfigError> {
    config(&[("BILLING", "grants")])
}

/// A signed-in account's user id.
fn account(
    h: &TestHost,
    cfg: &Result<slop_ninja_account_worker::Config, slop_ninja_account_worker::ConfigError>,
    addr: &str,
) -> String {
    let mut b = Browser::new();
    b.email_sign_in(h, cfg, addr);
    user_of(&mut b, h, cfg)
}

fn internal(
    h: &TestHost,
    cfg: &Result<slop_ninja_account_worker::Config, slop_ninja_account_worker::ConfigError>,
    path: &str,
    body: Value,
) -> (u16, Value) {
    let r = Browser::new().internal(h, cfg, path, body);
    (r.status, serde_json::from_slice(&r.body).unwrap_or(Value::Null))
}

fn hold(
    h: &TestHost,
    cfg: &Result<slop_ninja_account_worker::Config, slop_ninja_account_worker::ConfigError>,
    u: &str,
    tokens: u64,
    idem: &str,
) -> (u16, Value) {
    internal(
        h,
        cfg,
        "/internal/v1/billing/hold",
        json!({"user_id": u, "tokens": tokens, "channel": "paid_page", "idem": idem}),
    )
}

fn commit(
    h: &TestHost,
    cfg: &Result<slop_ninja_account_worker::Config, slop_ninja_account_worker::ConfigError>,
    hold_id: &str,
) -> (u16, Value) {
    internal(
        h,
        cfg,
        "/internal/v1/billing/commit",
        json!({"hold_id": hold_id, "receipt_id": "pr0123abcd", "package_pub": PKG}),
    )
}

fn release(
    h: &TestHost,
    cfg: &Result<slop_ninja_account_worker::Config, slop_ninja_account_worker::ConfigError>,
    hold_id: &str,
) -> (u16, Value) {
    internal(h, cfg, "/internal/v1/billing/release", json!({"hold_id": hold_id}))
}

fn peek(
    h: &TestHost,
    cfg: &Result<slop_ninja_account_worker::Config, slop_ninja_account_worker::ConfigError>,
    u: &str,
) -> (u16, Value) {
    internal(h, cfg, "/internal/v1/billing/peek", json!({"user_id": u}))
}

/// A plan grant's arguments: its days, its tokens, and its API part if
/// given.
fn plan_grant(u: &str, days: u64, tokens: u64, api: Option<u64>) -> Value {
    let mut a = json!({"user_id": u, "reason": "tester plan"});
    a["days"] = json!(days);
    a["allowance"] = json!(tokens);
    if let Some(x) = api {
        a["api_tokens"] = json!(x);
    }
    a
}

fn credit_left(h: &TestHost) -> i64 {
    h.count("SELECT COALESCE(SUM(tokens_left), 0) AS n FROM payments")
}

#[test]
fn a_grant_backs_a_hold_that_commits_once() {
    let h = TestHost::new();
    let cfg = grants();
    let u = account(&h, &cfg, "reader@example.com");
    let (s, a) = op(&h, &cfg, "grant_credit", json!({"user_id": u, "tokens": 5_000, "reason": "tester"}));
    assert_eq!(s, 200, "{a}");
    assert_eq!(a["tokens"], 5_000);
    assert_eq!(
        h.count("SELECT count(*) AS n FROM payments WHERE kind = 'grant' AND cash = 0 AND tokens_left = 5000"),
        1
    );
    // The payment names its journal entry.
    assert_eq!(h.count("SELECT count(*) AS n FROM payments p JOIN journal j ON j.seq = p.journal_seq WHERE j.op LIKE '%grant_credit%'"), 1);
    let (s, p) = peek(&h, &cfg, &u);
    assert_eq!(s, 200, "{p}");
    assert_eq!(
        (p["plan_left"].clone(), p["credit_left"].clone(), p["paid_preview"].clone()),
        (json!(0), json!(5_000), json!(false))
    );

    let (s, a) = hold(&h, &cfg, &u, 1_800, "q-one");
    assert_eq!(s, 200, "{a}");
    let placed: HoldAnswer = serde_json::from_value(a).unwrap();
    assert_eq!(placed.parts.len(), 1);
    assert!(!placed.parts[0].cash);
    assert_eq!(credit_left(&h), 3_200);
    // The same quote's confirm, sent again, places no second hold.
    let (_, again) = hold(&h, &cfg, &u, 1_800, "q-one");
    assert_eq!(again["hold_id"], json!(placed.hold_id));
    assert_eq!(credit_left(&h), 3_200);
    assert_eq!(h.count("SELECT count(*) AS n FROM holds"), 1);

    let (s, c) = commit(&h, &cfg, &placed.hold_id);
    assert_eq!((s, c["ended"].clone(), c["by_this_call"].clone()), (200, json!("committed"), json!(true)));
    assert_eq!(
        h.count("SELECT count(*) AS n FROM debits WHERE tokens = 1800 AND cash = 0 AND channel = 'paid_page'"),
        1
    );
    // A second commit, and a release after it, answer the end on record.
    let (_, c2) = commit(&h, &cfg, &placed.hold_id);
    assert_eq!((c2["ended"].clone(), c2["by_this_call"].clone()), (json!("committed"), json!(false)));
    let (_, r) = release(&h, &cfg, &placed.hold_id);
    assert_eq!((r["ended"].clone(), r["by_this_call"].clone()), (json!("committed"), json!(false)));
    assert_eq!(credit_left(&h), 3_200, "a release after a commit returns nothing");
    assert_eq!(h.count("SELECT count(*) AS n FROM debits"), 1);
    // The holder's billing page shows the grant and the debit.
    let mut b = Browser::new();
    b.email_sign_in(&h, &cfg, "reader@example.com");
    let v = b.view(&h, &cfg, "billing");
    let kinds: Vec<&str> = v["history"].as_array().unwrap().iter().map(|r| r["kind"].as_str().unwrap()).collect();
    assert!(kinds.contains(&"grant") && kinds.contains(&"debit"), "{v}");
    assert_eq!(v["credit_tokens"], 3_200);
}

#[test]
fn a_release_returns_the_parts_and_ends_the_hold_once() {
    let h = TestHost::new();
    let cfg = grants();
    let u = account(&h, &cfg, "reader@example.com");
    op(&h, &cfg, "grant_credit", json!({"user_id": u, "tokens": 1_000, "reason": "r"}));
    op(&h, &cfg, "grant_credit", json!({"user_id": u, "tokens": 1_000, "reason": "r"}));
    let (_, a) = hold(&h, &cfg, &u, 1_500, "q-two");
    let placed: HoldAnswer = serde_json::from_value(a).unwrap();
    // First paid, first used: all of the first grant, half of the second.
    let toks: Vec<u64> = placed.parts.iter().map(|p| p.tokens).collect();
    assert_eq!(toks, vec![1_000, 500]);
    assert_eq!(credit_left(&h), 500);
    let (s, r) = release(&h, &cfg, &placed.hold_id);
    assert_eq!((s, r["ended"].clone(), r["by_this_call"].clone()), (200, json!("released"), json!(true)));
    assert_eq!(credit_left(&h), 2_000);
    let (_, r2) = release(&h, &cfg, &placed.hold_id);
    assert_eq!((r2["ended"].clone(), r2["by_this_call"].clone()), (json!("released"), json!(false)));
    assert_eq!(credit_left(&h), 2_000, "a second release returns nothing");
    let (_, c) = commit(&h, &cfg, &placed.hold_id);
    assert_eq!(c["ended"], json!("released"), "a commit after a release debits nothing");
    assert_eq!(h.count("SELECT count(*) AS n FROM debits"), 0);
    assert_eq!(release(&h, &cfg, "h-no-such-hold").0, 404);
    // Short: 402, nothing written.
    let (s, a) = hold(&h, &cfg, &u, 2_001, "q-three");
    assert_eq!((s, a["code"].clone()), (402, json!("not_enough_credit")));
    assert_eq!(h.count("SELECT count(*) AS n FROM holds"), 1);
}

#[test]
fn an_ended_hold_s_idem_places_nothing_again() {
    let h = TestHost::new();
    let cfg = grants();
    let u = account(&h, &cfg, "reader@example.com");
    op(&h, &cfg, "grant_credit", json!({"user_id": u, "tokens": 5_000, "reason": "r"}));
    // Released (a start that failed after its hold gave it back): the quote
    // is spent, and a second confirm of it places nothing.
    let (_, a) = hold(&h, &cfg, &u, 1_000, "q-back");
    let id = a["hold_id"].as_str().unwrap().to_owned();
    assert_eq!(release(&h, &cfg, &id).1["ended"], json!("released"));
    assert_eq!(credit_left(&h), 5_000);
    let (s, r) = hold(&h, &cfg, &u, 1_000, "q-back");
    assert_eq!((s, r["code"].clone()), (409, json!("quote_invalid")));
    assert_eq!(credit_left(&h), 5_000);
    assert_eq!(h.count("SELECT count(*) AS n FROM holds"), 1);
    // Committed: the same, with the one debit on record.
    let (_, a) = hold(&h, &cfg, &u, 1_000, "q-done");
    let id = a["hold_id"].as_str().unwrap().to_owned();
    assert_eq!(commit(&h, &cfg, &id).1["ended"], json!("committed"));
    let (s, r) = hold(&h, &cfg, &u, 1_000, "q-done");
    assert_eq!((s, r["code"].clone()), (409, json!("quote_invalid")));
    assert_eq!(credit_left(&h), 4_000);
    assert_eq!(h.count("SELECT count(*) AS n FROM holds"), 2);
    assert_eq!(h.count("SELECT count(*) AS n FROM debits"), 1);
    // Another account naming that idem is still malformed.
    let v = account(&h, &cfg, "other@example.com");
    assert_eq!(hold(&h, &cfg, &v, 1_000, "q-done").1["code"], json!("malformed_request"));
    // A new quote's idem places as before.
    assert_eq!(hold(&h, &cfg, &u, 1_000, "q-new").0, 200);
    assert_eq!(credit_left(&h), 3_000);

    // The rollback path: between this request's read and its batch, another
    // request places this idem's hold and it is released. The batch fails on
    // the idem, and the re-read finds an ended hold: refused, not answered.
    let sw = block_on(Switches::load(cfg.as_ref().unwrap(), &h));
    let late = EndsFirst { h: &h, sw: &sw, req: req(&u, 1_000, "q-late"), done: Cell::new(false) };
    assert_eq!(block_on(ledger::place(&late, &sw, &late.req)), Err(RefusalCode::QuoteInvalid));
    assert!(late.done.get());
    assert_eq!(h.count("SELECT count(*) AS n FROM holds WHERE idem = 'q-late'"), 1);
    assert_eq!(credit_left(&h), 3_000, "nothing taken by the refused request");
}

#[test]
fn the_billing_switch_gates_new_holds_only() {
    let h = TestHost::new();
    let on = grants();
    let off = config(&[]);
    let live = config(&[("BILLING", "live")]);
    let u = account(&h, &on, "reader@example.com");
    op(&h, &on, "grant_credit", json!({"user_id": u, "tokens": 1_000, "reason": "r"}));
    let (_, a) = hold(&h, &on, &u, 200, "q-a");
    let id = a["hold_id"].as_str().unwrap().to_owned();
    assert_eq!(peek(&h, &off, &u).1["code"], json!("paid_off"));
    assert_eq!(hold(&h, &off, &u, 200, "q-b").1["code"], json!("paid_off"));
    assert_eq!(hold(&h, &live, &u, 200, "q-b").1["code"], json!("not_configured"), "live waits for the processor");
    // A hold placed before the flip still ends.
    assert_eq!(commit(&h, &off, &id).1["ended"], json!("committed"));
    // The operator's switch row: grants on under a var that allows it.
    let (s, _) = op(&h, &on, "set_switch", json!({"name": "BILLING", "state": "off", "reason": "pause"}));
    assert_eq!(s, 200);
    assert_eq!(hold(&h, &on, &u, 200, "q-c").1["code"], json!("paid_off"));
    op(&h, &on, "set_switch", json!({"name": "BILLING", "state": "grants", "reason": "resume"}));
    assert_eq!(hold(&h, &on, &u, 200, "q-c").0, 200);
    // Malformed: a free channel, no idem, nothing to hold.
    let bad = internal(
        &h,
        &on,
        "/internal/v1/billing/hold",
        json!({"user_id": u, "tokens": 200, "channel": "free_text", "idem": "q-d"}),
    );
    assert_eq!(bad.1["code"], json!("malformed_request"));
    assert_eq!(hold(&h, &on, &u, 0, "q-e").1["code"], json!("malformed_request"));
    assert_eq!(hold(&h, &on, &u, 10, "").1["code"], json!("malformed_request"));
}

#[test]
fn a_plan_draws_first_at_its_minimum_and_ends_without_rollover() {
    let h = TestHost::new();
    let cfg = grants();
    let u = account(&h, &cfg, "reader@example.com");
    let (s, a) = op(&h, &cfg, "grant_plan", plan_grant(&u, 30, 409_600, None));
    assert_eq!(s, 200, "{a}");
    assert_eq!(a["api_tokens"], 40_960);
    op(&h, &cfg, "grant_credit", json!({"user_id": u, "tokens": 1_000, "reason": "r"}));
    let (_, p) = peek(&h, &cfg, &u);
    assert_eq!(
        (p["plan_left"].clone(), p["api_left"].clone(), p["credit_left"].clone()),
        (json!(409_600), json!(40_960), json!(1_000))
    );
    // A short paid document draws the plan's minimum, and no credit.
    let (_, a) = hold(&h, &cfg, &u, 200, "q-plan-1");
    let placed: HoldAnswer = serde_json::from_value(a).unwrap();
    assert_eq!(placed.parts.len(), 1);
    assert_eq!((placed.parts[0].src.as_str(), placed.parts[0].tokens), ("period", 512));
    assert_eq!(credit_left(&h), 1_000);
    // The plan grant's backing payment holds no credit and shows no row of
    // its own; the plan shows as a plan grant.
    let mut b = Browser::new();
    b.email_sign_in(&h, &cfg, "reader@example.com");
    let v = b.view(&h, &cfg, "billing");
    let hist = v["history"].as_array().unwrap();
    assert_eq!(hist.iter().filter(|r| r["kind"] == "plan_grant").count(), 1, "{v}");
    assert!(hist.iter().all(|r| !(r["kind"] == "grant" && r["tokens"] == 0)), "{v}");
    // The period ends at its end: the hourly sweep closes it and a release
    // returns nothing into it.
    h.advance(31 * 86_400_000);
    block_on(slop_ninja_account_worker::sweep::run(&h, slop_ninja_account_worker::sweep::SESSIONS_CRON));
    let ended = h.count("SELECT used AS n FROM plan_periods WHERE state = 'ended' AND allowance = used");
    assert_eq!(ended, 512);
    release(&h, &cfg, &placed.hold_id);
    assert_eq!(h.count("SELECT used AS n FROM plan_periods"), 512);
    let (_, p) = peek(&h, &cfg, &u);
    assert_eq!(p["plan_left"], 0);
    // Credit takes over, at its own minimum.
    let (_, a) = hold(&h, &cfg, &u, 50, "q-plan-2");
    let placed: HoldAnswer = serde_json::from_value(a).unwrap();
    assert_eq!((placed.parts[0].src.as_str(), placed.parts[0].tokens), ("payment", 128));
    // Out-of-range figures are refused.
    for args in [
        plan_grant(&u, 0, 100, None),
        plan_grant(&u, 400, 100, None),
        plan_grant(&u, 30, 100, Some(200)),
        json!({"user_id": u, "days": 30, "reason": "r"}),
    ] {
        assert_eq!(op(&h, &cfg, "grant_plan", args).1["code"], json!("op_malformed"));
    }
}

#[test]
fn the_api_channel_draws_within_the_api_part() {
    let h = TestHost::new();
    let cfg = grants();
    let u = account(&h, &cfg, "reader@example.com");
    op(&h, &cfg, "grant_plan", plan_grant(&u, 30, 10_000, Some(1_000)));
    op(&h, &cfg, "grant_credit", json!({"user_id": u, "tokens": 5_000, "reason": "r"}));
    let (_, a) = internal(
        &h,
        &cfg,
        "/internal/v1/billing/hold",
        json!({"user_id": u, "tokens": 3_000, "channel": "api", "idem": "q-api"}),
    );
    let placed: HoldAnswer = serde_json::from_value(a).unwrap();
    let got: Vec<(&str, u64)> = placed.parts.iter().map(|p| (p.src.as_str(), p.tokens)).collect();
    assert_eq!(got, vec![("period", 1_000), ("payment", 2_000)]);
    assert_eq!(h.count("SELECT api_used AS n FROM plan_periods"), 1_000);
    release(&h, &cfg, &placed.hold_id);
    assert_eq!(h.count("SELECT api_used + used AS n FROM plan_periods"), 0);
}

#[test]
fn a_voided_grant_closes_and_its_holds_still_end() {
    let h = TestHost::new();
    let cfg = grants();
    let u = account(&h, &cfg, "reader@example.com");
    let (_, g) = op(&h, &cfg, "grant_credit", json!({"user_id": u, "tokens": 3_000, "reason": "r"}));
    let pid = g["payment_id"].as_u64().unwrap();
    let (_, a) = hold(&h, &cfg, &u, 1_000, "q-v1");
    let first = a["hold_id"].as_str().unwrap().to_owned();
    let (_, a) = hold(&h, &cfg, &u, 1_000, "q-v2");
    let second = a["hold_id"].as_str().unwrap().to_owned();
    let (s, v) = op(&h, &cfg, "void_grant", json!({"payment_id": pid, "reason": "abuse"}));
    assert_eq!((s, v["voided_tokens"].clone()), (200, json!(1_000)), "{v}");
    assert_eq!(h.count("SELECT count(*) AS n FROM payments WHERE state = 'void' AND tokens_left = 0"), 1);
    assert_eq!(peek(&h, &cfg, &u).1["credit_left"], 0);
    assert_eq!(hold(&h, &cfg, &u, 200, "q-v3").1["code"], json!("not_enough_credit"));
    // Placed holds end: a commit debits, a release returns nothing.
    assert_eq!(commit(&h, &cfg, &first).1["ended"], json!("committed"));
    assert_eq!(release(&h, &cfg, &second).1["ended"], json!("released"));
    assert_eq!(credit_left(&h), 0);
    // A second void, or a payment that is not a grant, finds nothing.
    assert_eq!(op(&h, &cfg, "void_grant", json!({"payment_id": pid, "reason": "again"})).1["code"], json!("not_found"));
    // A plan grant's void reverses its period.
    let (_, p) = op(&h, &cfg, "grant_plan", plan_grant(&u, 7, 5_000, None));
    let (_, v) = op(&h, &cfg, "void_grant", json!({"payment_id": p["payment_id"], "reason": "r"}));
    assert_eq!(v["periods_closed"], 1, "{v}");
    let reversed = h.count("SELECT count(*) AS n FROM plan_periods WHERE state = 'reversed' AND allowance = used");
    assert_eq!(reversed, 1);
    assert_eq!(peek(&h, &cfg, &u).1["plan_left"], 0);
}

#[test]
fn a_locked_or_unknown_account_holds_nothing() {
    let h = TestHost::new();
    let cfg = grants();
    let u = account(&h, &cfg, "reader@example.com");
    op(&h, &cfg, "grant_credit", json!({"user_id": u, "tokens": 1_000, "reason": "r"}));
    op(&h, &cfg, "disable_account", json!({"user_id": u, "reason": "abuse"}));
    assert_eq!(hold(&h, &cfg, &u, 200, "q-l").1["code"], json!("account_unavailable"));
    assert_eq!(peek(&h, &cfg, &u).1["code"], json!("account_unavailable"));
    let stranger = UserId([7; 16]).hex();
    assert_eq!(hold(&h, &cfg, &stranger, 200, "q-s").1["code"], json!("account_unavailable"));
    assert_eq!(
        op(&h, &cfg, "grant_credit", json!({"user_id": stranger, "tokens": 1, "reason": "r"})).1["code"],
        json!("not_found")
    );
    assert_eq!(h.count("SELECT count(*) AS n FROM holds"), 0);
}

#[test]
fn quotes_draw_on_the_account_quote_bucket() {
    let h = TestHost::new();
    let cfg = grants();
    let u = account(&h, &cfg, "reader@example.com");
    for _ in 0..30 {
        assert_eq!(peek(&h, &cfg, &u).0, 200);
    }
    let (s, a) = peek(&h, &cfg, &u);
    assert_eq!((s, a["code"].clone()), (429, json!("quota")));
    // The operator's path past it.
    let (s, _) = op(&h, &cfg, "reset_quota", json!({"kind": "quote", "key": u, "reason": "support"}));
    assert_eq!(s, 200);
    assert_eq!(peek(&h, &cfg, &u).0, 200);
    // One refills every two minutes.
    for _ in 0..29 {
        peek(&h, &cfg, &u);
    }
    assert_eq!(peek(&h, &cfg, &u).0, 429);
    h.advance(120_000);
    assert_eq!(peek(&h, &cfg, &u).0, 200);
    // A policy var that does not parse leaves the quote unconfigured.
    let bad = config(&[("BILLING", "grants"), ("BILLING_POLICY", "{\"version\":\"x\"}")]);
    assert_eq!(peek(&h, &bad, &u).1["code"], json!("not_configured"));
}

// --------------------------------------------------------------- the races

/// The test host, yielding once before every call, so a round-robin
/// executor interleaves several flows at each of their awaits.
struct Racing<'a>(&'a TestHost);

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

impl Host for Racing<'_> {
    async fn all(&self, stmt: Stmt) -> Result<Vec<Row>, DbError> {
        YieldOnce(false).await;
        self.0.all(stmt).await
    }
    async fn batch(&self, stmts: Vec<Stmt>) -> Result<Vec<Vec<Row>>, DbError> {
        YieldOnce(false).await;
        self.0.batch(stmts).await
    }
    async fn quota_take(&self, name: &str, size: TokenBucket) -> Result<QuotaTake, QuotaUnavailable> {
        self.0.quota_take(name, size).await
    }
    async fn quota_reset(&self, name: &str) -> Result<(), QuotaUnavailable> {
        self.0.quota_reset(name).await
    }
    async fn send_mail(&self, m: &Message) -> Result<(), SendFailure> {
        self.0.send_mail(m).await
    }
    async fn http(&self, req: &OutRequest) -> Result<OutReply, NetError> {
        self.0.http(req).await
    }
    async fn enqueue(&self, msg: &QueueMessage) -> Result<(), QueueUnavailable> {
        self.0.enqueue(msg).await
    }
    fn point(&self, point: &Point, blobs: &[&str]) {
        self.0.point(point, blobs)
    }
    fn fill(&self, buf: &mut [u8]) {
        self.0.fill(buf)
    }
    fn now_ms(&self) -> u64 {
        self.0.now_ms()
    }
}

/// Polls every future in turn until all are ready; answers their outputs in
/// order and how many rounds it took.
/// One flow in a race.
type Racer<'a, T> = Pin<Box<dyn Future<Output = T> + 'a>>;

fn race<T>(futs: Vec<Racer<'_, T>>) -> Vec<T> {
    let mut futs: Vec<Option<Racer<'_, T>>> = futs.into_iter().map(Some).collect();
    let mut out: Vec<Option<T>> = futs.iter().map(|_| None).collect();
    let mut cx = Context::from_waker(Waker::noop());
    let rounds = Cell::new(0);
    while out.iter().any(Option::is_none) {
        rounds.set(rounds.get() + 1);
        assert!(rounds.get() < 10_000, "the race never settles");
        for (i, f) in futs.iter_mut().enumerate() {
            if let Some(fut) = f
                && let Poll::Ready(v) = fut.as_mut().poll(&mut cx)
            {
                out[i] = Some(v);
                *f = None;
            }
        }
    }
    out.into_iter().map(Option::unwrap).collect()
}

/// The test host, on which another request places `req`'s hold and
/// releases it just before this request's own hold batch runs.
struct EndsFirst<'a> {
    h: &'a TestHost,
    sw: &'a Switches,
    req: HoldRequest,
    done: Cell<bool>,
}

impl Host for EndsFirst<'_> {
    async fn all(&self, stmt: Stmt) -> Result<Vec<Row>, DbError> {
        self.h.all(stmt).await
    }
    async fn batch(&self, stmts: Vec<Stmt>) -> Result<Vec<Vec<Row>>, DbError> {
        if !self.done.get() && stmts.iter().any(|s| s.sql.starts_with("INSERT INTO holds")) {
            self.done.set(true);
            let other = ledger::place(self.h, self.sw, &self.req).await.unwrap();
            ledger::end_hold(self.h, &other.hold_id, End::Release).await.unwrap();
        }
        self.h.batch(stmts).await
    }
    async fn quota_take(&self, name: &str, size: TokenBucket) -> Result<QuotaTake, QuotaUnavailable> {
        self.h.quota_take(name, size).await
    }
    async fn quota_reset(&self, name: &str) -> Result<(), QuotaUnavailable> {
        self.h.quota_reset(name).await
    }
    async fn send_mail(&self, m: &Message) -> Result<(), SendFailure> {
        self.h.send_mail(m).await
    }
    async fn http(&self, req: &OutRequest) -> Result<OutReply, NetError> {
        self.h.http(req).await
    }
    async fn enqueue(&self, msg: &QueueMessage) -> Result<(), QueueUnavailable> {
        self.h.enqueue(msg).await
    }
    fn point(&self, point: &Point, blobs: &[&str]) {
        self.h.point(point, blobs)
    }
    fn fill(&self, buf: &mut [u8]) {
        self.h.fill(buf)
    }
    fn now_ms(&self) -> u64 {
        self.h.now_ms()
    }
}

fn req(u: &str, tokens: u64, idem: &str) -> HoldRequest {
    HoldRequest { user_id: UserId::parse(u).unwrap(), tokens, channel: Channel::PaidPage, idem: idem.into() }
}

#[test]
fn twenty_holds_raced_against_a_balance_for_three() {
    let h = TestHost::new();
    let cfg = grants();
    let u = account(&h, &cfg, "reader@example.com");
    // 3,000 tokens in two grants: a hold may need to span both.
    op(&h, &cfg, "grant_credit", json!({"user_id": u, "tokens": 1_500, "reason": "r"}));
    op(&h, &cfg, "grant_credit", json!({"user_id": u, "tokens": 1_500, "reason": "r"}));
    let sw = block_on(Switches::load(cfg.as_ref().unwrap(), &h));
    let racing = Racing(&h);
    let reqs: Vec<HoldRequest> = (0..20).map(|i| req(&u, 1_000, &format!("q-race-{i}"))).collect();
    let futs: Vec<Racer<'_, Result<HoldAnswer, RefusalCode>>> =
        reqs.iter().map(|r| Box::pin(ledger::place(&racing, &sw, r)) as Racer<'_, _>).collect();
    let results = race(futs);
    let placed = results.iter().filter(|r| r.is_ok()).count() as i64;
    // The rule re-reads once after a rollback, so under this much contention
    // a hold may answer 402 while headroom is left; it never overdraws.
    assert!((1..=3).contains(&placed), "{results:?}");
    assert!(results.iter().all(|r| matches!(r, Ok(_) | Err(RefusalCode::NotEnoughCredit))));
    assert_eq!(h.count("SELECT count(*) AS n FROM holds"), placed, "a 402 wrote nothing");
    assert_eq!(h.count("SELECT COALESCE(SUM(tokens), 0) AS n FROM holds"), 1_000 * placed);
    assert_eq!(credit_left(&h), 3_000 - 1_000 * placed, "every placed hold's parts were deducted");
    // Every placed hold's parts sum to its tokens.
    for r in results.iter().flatten() {
        assert_eq!(ledger::total(&r.parts), 1_000);
    }
    // Sequentially, what is left is held to the last token and no further.
    for i in placed..3 {
        assert!(block_on(ledger::place(&h, &sw, &req(&u, 1_000, &format!("q-after-{i}")))).is_ok());
    }
    assert_eq!(block_on(ledger::place(&h, &sw, &req(&u, 1, "q-after-x"))), Err(RefusalCode::NotEnoughCredit));
    assert_eq!(credit_left(&h), 0);
    assert_eq!(h.count("SELECT count(*) AS n FROM holds"), 3);
}

#[test]
fn one_quote_confirmed_five_times_at_once_places_one_hold() {
    let h = TestHost::new();
    let cfg = grants();
    let u = account(&h, &cfg, "reader@example.com");
    op(&h, &cfg, "grant_credit", json!({"user_id": u, "tokens": 10_000, "reason": "r"}));
    let sw = block_on(Switches::load(cfg.as_ref().unwrap(), &h));
    let racing = Racing(&h);
    let reqs: Vec<HoldRequest> = (0..5).map(|_| req(&u, 2_000, "q-same")).collect();
    let futs: Vec<Racer<'_, Result<HoldAnswer, RefusalCode>>> =
        reqs.iter().map(|r| Box::pin(ledger::place(&racing, &sw, r)) as Racer<'_, _>).collect();
    let results = race(futs);
    let ids: Vec<String> = results.iter().map(|r| r.as_ref().unwrap().hold_id.clone()).collect();
    assert!(ids.iter().all(|i| *i == ids[0]), "{ids:?}");
    assert_eq!(h.count("SELECT count(*) AS n FROM holds"), 1);
    assert_eq!(credit_left(&h), 8_000);
}

#[test]
fn a_commit_raced_against_a_release_ends_the_hold_once() {
    for commit_first in [true, false] {
        let h = TestHost::new();
        let cfg = grants();
        let u = account(&h, &cfg, "reader@example.com");
        op(&h, &cfg, "grant_credit", json!({"user_id": u, "tokens": 2_000, "reason": "r"}));
        let (_, a) = hold(&h, &cfg, &u, 1_000, "q-end");
        let id = a["hold_id"].as_str().unwrap().to_owned();
        let racing = Racing(&h);
        let c = Box::pin(ledger::end_hold(&racing, &id, End::Commit { receipt_id: "pr-1", package_pub: PKG }));
        let r = Box::pin(ledger::end_hold(&racing, &id, End::Release));
        let futs: Vec<Racer<'_, _>> = if commit_first { vec![c, r] } else { vec![r, c] };
        let results = race(futs);
        let ends: Vec<_> = results.iter().map(|x| x.as_ref().unwrap().ended).collect();
        assert_eq!(ends[0], ends[1], "both calls answer the one end");
        assert_eq!(results.iter().filter(|x| x.as_ref().unwrap().by_this_call).count(), 1);
        assert_eq!(h.count("SELECT count(*) AS n FROM hold_ends"), 1);
        match ends[0] {
            HoldEnd::Committed => {
                assert_eq!(h.count("SELECT count(*) AS n FROM debits"), 1);
                assert_eq!(credit_left(&h), 1_000);
            }
            HoldEnd::Released => {
                assert_eq!(h.count("SELECT count(*) AS n FROM debits"), 0);
                assert_eq!(credit_left(&h), 2_000);
            }
        }
    }
}
