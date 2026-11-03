//! End-to-end heartbeat tests: deliveries, cron ticks and RPC rounds run
//! against native SQLite and a fake finalized chain, with a driver that plays
//! the Worker's part (execute a plan, hand the result back, stop at the round
//! bound).

use super::*;
use crate::journal::Halt;
use crate::sql::{CHAIN_JOURNAL_SCHEMA, migrate};
use crate::testing::{Sqlite, chain_hash, note, tx};
use std::collections::HashMap;

const T0: u64 = 1_793_000_000_000;
const MIN: u64 = 60_000;

fn journal() -> Sqlite {
    let db = Sqlite::open();
    tx(&db, |sql| migrate(sql, &CHAIN_JOURNAL_SCHEMA)).unwrap();
    db
}

#[derive(Default)]
struct Chain {
    head: u64,
    calls: u64,
    forks: HashMap<u64, Id>,
}

impl Chain {
    fn at(head: u64) -> Self {
        Self {
            head,
            ..Self::default()
        }
    }

    fn hash(&self, n: u64) -> Option<Id> {
        (n <= self.head).then(|| self.forks.get(&n).copied().unwrap_or(chain_hash(n)))
    }

    fn header(&self, n: u64) -> FinalizedHeader {
        FinalizedHeader {
            number: n,
            hash: self.forks.get(&n).copied().unwrap_or(chain_hash(n)),
            parent_hash: self
                .forks
                .get(&(n - 1))
                .copied()
                .unwrap_or(chain_hash(n - 1)),
        }
    }

    fn execute(&mut self, plan: &RpcPlan) -> RpcResult {
        self.calls += plan.calls();
        match plan.clone() {
            RpcPlan::FinalizedHead {
                purpose,
                origin,
                claim,
            } => RpcResult::FinalizedHead {
                purpose,
                origin,
                claim,
                head: Ok(self.header(self.head)),
            },
            RpcPlan::BlockHashes {
                from,
                to,
                origin,
                claim,
            } => RpcResult::BlockHashes {
                from,
                to,
                origin,
                claim,
                hashes: Ok((from..=to).map(|n| self.hash(n)).collect()),
            },
        }
    }
}

fn ident(fill: u8, n: u64) -> Id {
    let mut bytes = [fill; 32];
    bytes[..8].copy_from_slice(&n.to_be_bytes());
    Id(bytes)
}

/// Delivery of block `n`; `generation` distinguishes re-registrations.
fn notification(chain: &Chain, n: u64, generation: u8) -> BlockNotification {
    BlockNotification {
        delivery_id: ident(0xd0 + generation, n),
        event_id: ident(0xe0 + generation, n),
        body_sha256: ident(0xb0 + generation, n),
        ..note(0, 0, 0, chain.header(n))
    }
}

/// Plays the Worker: runs `plan` and its follow-ups up to `rounds` rounds.
fn drive(
    db: &Sqlite,
    s: &Settings,
    chain: &mut Chain,
    plan: Option<RpcPlan>,
    rounds: u64,
    holder: &str,
    now: u64,
) -> Vec<Step> {
    let mut steps = Vec::new();
    let mut plan = plan;
    let mut round = 0;
    while let Some(p) = plan.take() {
        round += 1;
        let result = chain.execute(&p);
        let step = tx(db, |sql| {
            on_rpc_result(sql, s, &result, round < rounds, holder, now)
        })
        .unwrap();
        plan = step.plan.clone();
        steps.push(step);
    }
    assert!(round <= rounds);
    steps
}

/// One delivery invocation: persist, acknowledge, then the bounded RPC rounds.
fn deliver(db: &Sqlite, s: &Settings, chain: &mut Chain, n: u64, now: u64) -> DeliveryReport {
    deliver_as(db, s, chain, n, 0, now)
}

fn deliver_as(
    db: &Sqlite,
    s: &Settings,
    chain: &mut Chain,
    n: u64,
    generation: u8,
    now: u64,
) -> DeliveryReport {
    let note = notification(chain, n, generation);
    let report = tx(db, |sql| on_delivery(sql, s, &note, now)).unwrap();
    drive(
        db,
        s,
        chain,
        report.plan.clone(),
        s.delivery_rounds,
        "delivery",
        now,
    );
    report
}

fn tick(db: &Sqlite, s: &Settings, chain: &mut Chain, now: u64) -> (TickReport, Vec<Step>) {
    let report = tx(db, |sql| on_tick(sql, s, now)).unwrap();
    let steps = drive(
        db,
        s,
        chain,
        report.plan.clone(),
        s.tick_rounds,
        "tick",
        now,
    );
    (report, steps)
}

fn status_at(db: &Sqlite, s: &Settings, now: u64) -> Status {
    tx(db, |sql| status(sql, s, 3, now)).unwrap()
}

fn counter(db: &Sqlite, name: &str) -> u64 {
    tx(db, |sql| journal::counter(sql, name)).unwrap()
}

fn cursor_number(db: &Sqlite) -> Option<u64> {
    tx(db, journal::cursor).unwrap().map(|c| c.number)
}

#[test]
fn first_delivery_anchors_then_deliveries_without_gaps_make_no_rpc_calls() {
    let db = journal();
    let s = Settings::default();
    let mut chain = Chain::at(100);
    let first = deliver(&db, &s, &mut chain, 100, T0);
    assert!(matches!(
        first.plan,
        Some(RpcPlan::FinalizedHead {
            purpose: HeadPurpose::Anchor,
            ..
        })
    ));
    assert_eq!((chain.calls, cursor_number(&db)), (2, Some(100)));
    for n in 101..=120 {
        chain.head = n;
        let r = deliver(&db, &s, &mut chain, n, T0 + (n - 100) * 12_000);
        assert_eq!(r.plan, None, "block {n}");
        assert_eq!(r.progress.applied, 1);
    }
    assert_eq!((chain.calls, cursor_number(&db)), (2, Some(120)));
    let st = status_at(&db, &s, T0 + 240_000);
    assert_eq!((st.mode, st.journal.events_applied), (Mode::Primary, 20));
    assert_eq!(st.backfill_claim.unwrap().state, "released");
}

#[test]
fn an_anchor_below_the_delivery_backfills_the_gap_first() {
    let db = journal();
    let s = Settings::default();
    // The RPC head lags the provider: the anchor lands below the delivery.
    let mut chain = Chain::at(97);
    let r = deliver(&db, &s, &mut chain, 100, T0);
    assert!(r.plan.is_some());
    // Round 1 anchored at 97; round 2 found 98..99 not yet on the node.
    assert_eq!(cursor_number(&db), Some(97));
    chain.head = 101;
    deliver(&db, &s, &mut chain, 101, T0 + 12_000);
    assert_eq!(cursor_number(&db), Some(101));
}

#[test]
fn gaps_backfill_in_bounded_rounds_and_carry_over() {
    let db = journal();
    let s = Settings {
        hashes_per_call: 8,
        ..Settings::default()
    };
    let mut chain = Chain::at(100);
    deliver(&db, &s, &mut chain, 100, T0);
    chain.calls = 0;
    chain.head = 140;
    // Webhooks resume after a pause: block 130 arrives with 101..129 missing.
    let r = deliver(&db, &s, &mut chain, 130, T0 + MIN);
    assert_eq!(r.progress.halt, Some(Halt::Gap { from: 101, to: 129 }));
    assert_eq!(
        r.plan,
        Some(RpcPlan::BlockHashes {
            from: 100,
            to: 107,
            origin: Origin::Delivery,
            claim: ClaimToken {
                op_id: BACKFILL_OP.into(),
                generation: 2
            }
        })
    );
    // Two rounds of 7 new heights each; the rest carries over.
    assert_eq!((chain.calls, cursor_number(&db)), (2, Some(114)));
    deliver(&db, &s, &mut chain, 131, T0 + MIN + 12_000);
    assert_eq!((chain.calls, cursor_number(&db)), (4, Some(128)));
    deliver(&db, &s, &mut chain, 132, T0 + MIN + 24_000);
    // 129 arrives by RPC, then the pending 130..132 deliveries apply in order.
    assert_eq!((chain.calls, cursor_number(&db)), (5, Some(132)));
    assert_eq!(counter(&db, "applied_rpc"), 29);
    assert_eq!(counter(&db, "applied_webhook"), 3);
}

#[test]
fn overlapping_invocations_skip_claimed_backfill_and_stale_holders_are_fenced() {
    let db = journal();
    let s = Settings::default();
    let mut chain = Chain::at(100);
    deliver(&db, &s, &mut chain, 100, T0);
    chain.head = 200;
    // Invocation A plans a backfill round but has not run it yet.
    let a = notification(&chain, 150, 0);
    let a_report = tx(&db, |sql| on_delivery(sql, &s, &a, T0 + 1_000)).unwrap();
    let a_plan = a_report.plan.clone().unwrap();
    // The next block's invocation overlaps and skips the claimed work.
    let b = notification(&chain, 151, 0);
    let b_report = tx(&db, |sql| on_delivery(sql, &s, &b, T0 + 13_000)).unwrap();
    assert_eq!(b_report.plan, None);
    assert_eq!(counter(&db, "backfill_claim_skips"), 1);
    // A crashed. After its lease expires a later block re-claims the work.
    let later = T0 + 1_000 + s.lease_ms + 1;
    let c = notification(&chain, 152, 0);
    let c_report = tx(&db, |sql| on_delivery(sql, &s, &c, later)).unwrap();
    let RpcPlan::BlockHashes { claim, .. } = c_report.plan.clone().unwrap() else {
        panic!()
    };
    assert_eq!(claim.generation, 3);
    // A's late result is fenced and changes nothing.
    let stale = chain.execute(&a_plan);
    let step = tx(&db, |sql| on_rpc_result(sql, &s, &stale, true, "a", later)).unwrap();
    assert!(step.fenced);
    assert_eq!(
        (cursor_number(&db), counter(&db, "fenced_results")),
        (Some(100), 1)
    );
    drive(
        &db,
        &s,
        &mut chain,
        c_report.plan,
        s.delivery_rounds,
        "c",
        later,
    );
    // Round 1 fetched 101..131, round 2 closed the gap to 149, and the pending
    // deliveries 150..152 then applied in order. A's stale round is counted:
    // anchor (2), A (1, wasted), C (2).
    assert_eq!(cursor_number(&db), Some(152));
    assert_eq!(chain.calls, 2 + 1 + 2);
}

#[test]
fn silence_enters_fallback_and_ticks_walk_the_cursor_to_the_head() {
    let db = journal();
    let s = Settings::default();
    let mut chain = Chain::at(100);
    deliver(&db, &s, &mut chain, 100, T0);
    // Webhooks stop. The chain keeps finalizing one block per 12 s.
    for minute in 1..=4 {
        chain.head = 100 + minute * 5;
        let (report, _) = tick(&db, &s, &mut chain, T0 + minute * MIN);
        assert!(!report.fallback, "minute {minute}");
        // Primary: one head read per tick and no backfill.
        assert_eq!(cursor_number(&db), Some(100));
    }
    assert_eq!(chain.calls, 2 + 4 * 2);
    let mut now = T0 + 5 * MIN;
    chain.head = 125;
    let (report, steps) = tick(&db, &s, &mut chain, now);
    assert!(report.fallback);
    // Head read plus one hash-list round reaches the head.
    assert_eq!((steps.len(), cursor_number(&db)), (2, Some(125)));
    // Ticks keep following the head while webhooks stay silent.
    for minute in 6..=8 {
        now = T0 + minute * MIN;
        chain.head = 100 + minute * 5;
        let (report, _) = tick(&db, &s, &mut chain, now);
        assert!(report.fallback);
        assert_eq!(cursor_number(&db), Some(chain.head));
    }
    let st = status_at(&db, &s, now);
    assert_eq!(
        (st.mode, st.mode_reason.as_deref()),
        (Mode::Fallback, Some("webhook_silent"))
    );
    assert_eq!(st.journal.lag_blocks, Some(0));

    // Webhooks resume, redelivering blocks the fallback already applied.
    let applied = counter(&db, "applied_rpc");
    for n in 139..=140 {
        let r = deliver(&db, &s, &mut chain, n, now + 1_000);
        assert_eq!(r.outcome, DeliveryOutcome::DuplicateBlock);
        assert_eq!(r.plan, None);
    }
    assert_eq!(counter(&db, "applied_rpc"), applied);
    chain.head = 141;
    let r = deliver(&db, &s, &mut chain, 141, now + 12_000);
    assert_eq!(
        (r.outcome, r.progress.applied),
        (DeliveryOutcome::Accepted, 1)
    );
    let st = status_at(&db, &s, now + 12_000);
    assert_eq!(
        (st.mode, st.mode_reason.as_deref()),
        (Mode::Primary, Some("webhook_resumed"))
    );
    assert_eq!(st.transitions, 2);
    let reasons: Vec<_> = st
        .recent_transitions
        .iter()
        .map(|t| (t.to_mode.as_str(), t.reason.as_str()))
        .collect();
    assert_eq!(
        reasons,
        [
            ("primary", "webhook_resumed"),
            ("fallback", "webhook_silent")
        ]
    );
}

#[test]
fn fallback_rounds_are_bounded_per_tick() {
    let db = journal();
    let s = Settings::default();
    let mut chain = Chain::at(100);
    deliver(&db, &s, &mut chain, 100, T0);
    tick(&db, &s, &mut chain, T0 + MIN);
    chain.head = 1_000;
    chain.calls = 0;
    let (_, steps) = tick(&db, &s, &mut chain, T0 + 6 * MIN);
    // One head read and three hash lists of 31 new heights each.
    assert_eq!((steps.len() as u64, chain.calls), (s.tick_rounds, 5));
    assert_eq!(cursor_number(&db), Some(100 + 3 * 31));
    assert!(chain.calls <= s.rpc_calls_per_minute);
    // The remainder carries over to the next tick.
    tick(&db, &s, &mut chain, T0 + 7 * MIN);
    assert_eq!(cursor_number(&db), Some(100 + 6 * 31));
}

#[test]
fn a_lagging_cursor_with_live_webhooks_enters_fallback_until_caught_up() {
    let db = journal();
    let s = Settings::default();
    let mut chain = Chain::at(100);
    deliver(&db, &s, &mut chain, 100, T0);
    // The provider is delivering, but far behind the finalized head.
    chain.head = 160;
    deliver(&db, &s, &mut chain, 101, T0 + 30_000);
    let (report, _) = tick(&db, &s, &mut chain, T0 + MIN);
    assert!(!report.fallback);
    let st = status_at(&db, &s, T0 + MIN);
    assert_eq!(
        (st.mode, st.mode_reason.as_deref()),
        (Mode::Fallback, Some("behind"))
    );
    // The same tick already walked toward the head.
    assert_eq!(cursor_number(&db), Some(160));
    deliver(&db, &s, &mut chain, 102, T0 + MIN + 10_000);
    assert_eq!(status_at(&db, &s, T0 + MIN + 10_000).mode, Mode::Fallback);
    chain.head = 165;
    tick(&db, &s, &mut chain, T0 + 2 * MIN);
    let st = status_at(&db, &s, T0 + 2 * MIN);
    assert_eq!(
        (st.mode, st.mode_reason.as_deref()),
        (Mode::Primary, Some("caught_up"))
    );
    assert!(!st.behind);
}

#[test]
fn ticks_read_the_head_at_most_once_and_stay_within_the_budget() {
    let db = journal();
    let s = Settings {
        rpc_calls_per_minute: 3,
        ..Settings::default()
    };
    let mut chain = Chain::at(100);
    let (first, _) = tick(&db, &s, &mut chain, T0);
    assert!(first.plan.is_some());
    // The tick anchored on the head it read.
    assert_eq!(cursor_number(&db), Some(100));
    let (dup, _) = tick(&db, &s, &mut chain, T0 + 5_000);
    assert_eq!(dup.plan, None);
    assert_eq!(counter(&db, "duplicate_ticks"), 1);
    chain.head = 150;
    // One call left this minute: one gap round, then the budget defers.
    deliver(&db, &s, &mut chain, 150, T0 + 10_000);
    assert_eq!(chain.calls, 3);
    let r = deliver(&db, &s, &mut chain, 149, T0 + 20_000);
    assert_eq!(r.plan, None);
    assert!(counter(&db, "rpc_budget_deferrals") >= 1);
    assert_eq!(chain.calls, 3);
    let st = status_at(&db, &s, T0 + 20_000);
    assert_eq!(st.rpc_calls_this_minute, 3);
    // The deferred claim was released, so the next minute proceeds at once.
    let r = deliver(&db, &s, &mut chain, 148, T0 + MIN + 1);
    assert!(r.plan.is_some());
}

#[test]
fn a_fault_pauses_deliveries_and_ticks_until_cleared() {
    let db = journal();
    let s = Settings::default();
    let mut chain = Chain::at(100);
    deliver(&db, &s, &mut chain, 100, T0);
    // A delivery whose parent is not the cursor.
    let mut bad = notification(&chain, 101, 0);
    bad.header.parent_hash = ident(0x77, 100);
    let r = tx(&db, |sql| on_delivery(sql, &s, &bad, T0 + 12_000)).unwrap();
    let Some(Halt::Fault { id: fault }) = r.progress.halt else {
        panic!("{r:?}")
    };
    assert_eq!(r.plan, None);
    // Later deliveries persist and acknowledge but do not move the cursor.
    chain.head = 103;
    let r = deliver(&db, &s, &mut chain, 103, T0 + 36_000);
    assert_eq!((r.outcome, r.plan), (DeliveryOutcome::Accepted, None));
    let (t, _) = tick(&db, &s, &mut chain, T0 + 6 * MIN);
    assert!(t.paused && t.plan.is_none());
    let st = status_at(&db, &s, T0 + 6 * MIN);
    assert_eq!(st.journal.fault.as_ref().unwrap().kind, "parent_mismatch");
    assert_eq!(cursor_number(&db), Some(100));
    let (outcome, progress) =
        tx(&db, |sql| clear_fault(sql, &s, fault, false, T0 + 7 * MIN)).unwrap();
    assert_eq!(outcome, journal::ClearOutcome::Cleared { reanchor: false });
    // 101 and 102 are missing; the next delivery backfills them.
    assert_eq!(progress.halt, Some(Halt::Gap { from: 101, to: 102 }));
    deliver(&db, &s, &mut chain, 104, T0 + 7 * MIN);
    assert_eq!(cursor_number(&db), Some(104));
}

#[test]
fn each_new_delivery_drives_one_scheduling_step() {
    let db = journal();
    let s = Settings::default();
    let mut chain = Chain::at(100);
    deliver(&db, &s, &mut chain, 100, T0);
    // The delivery's own step, then the anchor round that confirmed 100.
    assert_eq!(counter(&db, "schedule_steps"), 2);
    // A provider retry of the same delivery drives nothing.
    let retry = BlockNotification {
        attempt: 2,
        ..notification(&chain, 100, 0)
    };
    let r = tx(&db, |sql| on_delivery(sql, &s, &retry, T0 + 1_000)).unwrap();
    assert_eq!(r.outcome, DeliveryOutcome::DuplicateDelivery);
    assert_eq!(counter(&db, "schedule_steps"), 2);
    // Re-registration redelivers the block under a new delivery ID.
    deliver_as(&db, &s, &mut chain, 100, 1, T0 + 2_000);
    assert_eq!(counter(&db, "schedule_steps"), 3);

    // The step claims due work; an overlapping holder skips it.
    let due = vec!["cohort/7/score".to_owned()];
    let held = tx(&db, |sql| {
        schedule::step(sql, &due, "block-101", 25_000, T0)
    })
    .unwrap();
    assert_eq!(held.len(), 1);
    let overlap = tx(&db, |sql| {
        schedule::step(sql, &due, "block-102", 25_000, T0 + 12_000)
    })
    .unwrap();
    assert!(overlap.is_empty());
    assert!(tx(&db, |sql| claims::complete(sql, &held[0], T0 + 20_000)).unwrap());
    let after = tx(&db, |sql| {
        schedule::step(sql, &due, "block-110", 25_000, T0 + 120_000)
    })
    .unwrap();
    assert!(after.is_empty());
}

#[test]
fn status_is_content_free() {
    let db = journal();
    let s = Settings::default();
    let mut chain = Chain::at(100);
    let n = notification(&chain, 100, 0);
    deliver(&db, &s, &mut chain, 100, T0);
    let json = serde_json::to_string(&status_at(&db, &s, T0)).unwrap();
    for secret_ish in [
        n.delivery_id.to_hex(),
        n.event_id.to_hex(),
        n.subscription.to_hex(),
        n.body_sha256.to_hex(),
    ] {
        assert!(!json.contains(&secret_ish), "{json}");
    }
    assert!(json.contains("\"mode\":\"primary\""));
}

#[test]
fn scheduling_waits_for_the_watchdog_audit_to_confirm_webhook_blocks() {
    let db = journal();
    let s = Settings::default();
    let mut chain = Chain::at(100);
    deliver(&db, &s, &mut chain, 100, T0);
    for n in 101..=105 {
        chain.head = n;
        let r = deliver(&db, &s, &mut chain, n, T0 + (n - 100) * 12_000);
        // The delivery advances the cursor; scheduling still sees 100.
        assert_eq!(r.progress.cursor.map(|c| c.number), Some(n));
        assert_eq!(r.progress.confirmed.map(|c| c.number), Some(100));
    }
    let calls = chain.calls;
    let steps = counter(&db, "schedule_steps");
    let st = status_at(&db, &s, T0 + MIN);
    assert_eq!(st.journal.provisional_blocks, Some(5));
    // The tick's one head read confirms the provider's chain up to the head
    // and drives a scheduling step at the new watermark, with no extra RPC.
    let (_, rounds) = tick(&db, &s, &mut chain, T0 + MIN);
    assert_eq!(
        rounds.last().unwrap().progress.confirmed.map(|c| c.number),
        Some(105)
    );
    assert_eq!(chain.calls, calls + 2);
    assert_eq!(counter(&db, "schedule_steps"), steps + 1);
    let st = status_at(&db, &s, T0 + MIN);
    assert_eq!(st.journal.provisional_blocks, Some(0));
    // A head the provider has not delivered yet confirms on arrival.
    chain.head = 107;
    tick(&db, &s, &mut chain, T0 + 2 * MIN);
    assert_eq!(
        status_at(&db, &s, T0 + 2 * MIN)
            .journal
            .confirmed
            .map(|c| c.number),
        Some(105)
    );
    deliver(&db, &s, &mut chain, 106, T0 + 2 * MIN + 1_000);
    let r = deliver(&db, &s, &mut chain, 107, T0 + 2 * MIN + 2_000);
    assert_eq!(r.progress.confirmed.map(|c| c.number), Some(107));
}

#[test]
fn fallback_blocks_from_rpc_are_confirmed_as_they_apply() {
    let db = journal();
    let s = Settings::default();
    let mut chain = Chain::at(100);
    deliver(&db, &s, &mut chain, 100, T0);
    chain.head = 130;
    let (report, _) = tick(&db, &s, &mut chain, T0 + 6 * MIN);
    assert!(report.fallback);
    let st = status_at(&db, &s, T0 + 6 * MIN);
    assert_eq!(st.journal.cursor.map(|c| c.number), Some(130));
    assert_eq!(st.journal.confirmed.map(|c| c.number), Some(130));
    assert_eq!(st.journal.provisional_blocks, Some(0));
}
