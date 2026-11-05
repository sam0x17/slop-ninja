//! Contract `rtao-block-webhooks/2` timing: a delivery is pushed within
//! about a second of finality, possibly before the RPC endpoint the Worker
//! reads serves (or finalizes) that block. These tests drive the heartbeat
//! against a mocked RPC that lags the webhook by 0 to 3 blocks and check
//! that every not-yet-served answer (a `null` header, a `null` height in a
//! hash list, a stale finalized head, a best chain on another fork above
//! it) is a bounded retry or a deferral to the next delivery or tick: never
//! a gap, a fault, a skipped height or a block applied twice. Signing, the
//! cohort kicks and the commitment reader read the chain only at the
//! confirmed watermark, so the invariant `confirmed <= the RPC finalized
//! head` (checked after every step) is what keeps their reads on blocks the
//! endpoint serves.

use super::*;
use crate::journal::Halt;
use crate::sql::{CHAIN_JOURNAL_SCHEMA, migrate};
use crate::testing::{Sqlite, chain_hash, note, tx};
use crate::webhook::Contract;

const T0: u64 = 1_793_000_000_000;
const BLOCK: u64 = 12_000;

fn journal() -> Sqlite {
    let db = Sqlite::open();
    tx(&db, |sql| migrate(sql, &CHAIN_JOURNAL_SCHEMA)).unwrap();
    db
}

/// The hash a fork above the RPC's finalized head would carry at `n`.
fn fork_hash(n: u64) -> Id {
    let mut bytes = [0xf0; 32];
    bytes[..8].copy_from_slice(&n.to_be_bytes());
    Id(bytes)
}

fn ident(fill: u8, n: u64) -> Id {
    let mut bytes = [fill; 32];
    bytes[..8].copy_from_slice(&n.to_be_bytes());
    Id(bytes)
}

/// A mocked RPC endpoint behind the webhook provider. The provider has
/// finalized `final_head`; the endpoint has finalized `final_head - lag` and
/// imported (its best chain) up to `final_head`. Above its finalized head
/// its best chain is another fork when `fork_above_final` is set.
struct LaggingRpc {
    final_head: u64,
    lag: u64,
    fork_above_final: bool,
    /// The next `null_headers` header reads answer `null` (a lagging node
    /// behind a load balancer).
    null_headers: u32,
    /// Hash lists answer `null` above this height (a lagging node), when set.
    lists_serve_to: Option<u64>,
    calls: u64,
    /// The highest height any hash list asked for.
    max_listed: u64,
}

impl LaggingRpc {
    fn new(final_head: u64, lag: u64) -> Self {
        Self {
            final_head,
            lag,
            fork_above_final: false,
            null_headers: 0,
            lists_serve_to: None,
            calls: 0,
            max_listed: 0,
        }
    }

    fn rpc_final(&self) -> u64 {
        self.final_head.saturating_sub(self.lag)
    }

    /// The endpoint's best-chain hash at `n`, `None` if it has not imported it.
    fn best_hash(&self, n: u64) -> Option<Id> {
        if n > self.final_head || self.lists_serve_to.is_some_and(|top| n > top) {
            return None;
        }
        Some(if self.fork_above_final && n > self.rpc_final() { fork_hash(n) } else { chain_hash(n) })
    }

    fn execute(&mut self, plan: &RpcPlan) -> RpcResult {
        self.calls += plan.calls();
        match plan.clone() {
            RpcPlan::FinalizedHead { purpose, origin, claim } => {
                let head = if self.null_headers > 0 {
                    self.null_headers -= 1;
                    Err(RpcError::NotServed)
                } else {
                    let n = self.rpc_final();
                    Ok(FinalizedHeader { number: n, hash: chain_hash(n), parent_hash: chain_hash(n - 1) })
                };
                RpcResult::FinalizedHead { purpose, origin, claim, head }
            }
            RpcPlan::BlockHashes { from, to, origin, claim } => {
                self.max_listed = self.max_listed.max(to);
                RpcResult::BlockHashes {
                    from,
                    to,
                    origin,
                    claim,
                    hashes: Ok((from..=to).map(|n| self.best_hash(n)).collect()),
                }
            }
        }
    }
}

/// The provider's delivery of finalized block `n` (attempt `attempt`).
fn delivery(n: u64, attempt: u16, contract: Contract) -> BlockNotification {
    BlockNotification {
        delivery_id: ident(0xd0, n),
        event_id: ident(0xe0, n),
        body_sha256: ident(0xb0, n),
        attempt,
        contract,
        ..note(0, 0, 0, FinalizedHeader { number: n, hash: chain_hash(n), parent_hash: chain_hash(n - 1) })
    }
}

/// Plays the Worker: runs `plan` and its follow-ups up to `rounds` rounds.
fn drive(db: &Sqlite, s: &Settings, rpc: &mut LaggingRpc, plan: Option<RpcPlan>, rounds: u64, now: u64) -> u64 {
    let mut plan = plan;
    let mut round = 0;
    while let Some(p) = plan.take() {
        round += 1;
        let result = rpc.execute(&p);
        let step = tx(db, |sql| on_rpc_result(sql, s, &result, round < rounds, "lag", now)).unwrap();
        plan = step.plan;
    }
    assert!(round <= rounds);
    round
}

fn deliver(db: &Sqlite, s: &Settings, rpc: &mut LaggingRpc, n: BlockNotification, now: u64) -> (DeliveryReport, u64) {
    let report = tx(db, |sql| on_delivery(sql, s, &n, now)).unwrap();
    let rounds = drive(db, s, rpc, report.plan.clone(), s.delivery_rounds, now);
    (report, rounds)
}

fn tick(db: &Sqlite, s: &Settings, rpc: &mut LaggingRpc, now: u64) -> TickReport {
    let report = tx(db, |sql| on_tick(sql, s, now)).unwrap();
    drive(db, s, rpc, report.plan.clone(), s.tick_rounds, now);
    report
}

fn counter(db: &Sqlite, name: &str) -> u64 {
    tx(db, |sql| journal::counter(sql, name)).unwrap()
}

fn cursor(db: &Sqlite) -> Option<BlockRef> {
    tx(db, journal::cursor).unwrap()
}

fn confirmed(db: &Sqlite) -> Option<BlockRef> {
    tx(db, journal::confirmed).unwrap()
}

/// What must hold after every step, whatever the lag: no fault; the cursor
/// and the confirmed watermark on the provider's finalized chain; the
/// watermark never above the endpoint's finalized head (so every read at it
/// names a block the endpoint serves) and never moving down.
fn check(db: &Sqlite, rpc: &LaggingRpc, floor: &mut u64, at: &str) {
    assert_eq!(tx(db, journal::active_fault).unwrap(), None, "{at}: no fault");
    if let Some(c) = cursor(db) {
        assert_eq!(c.hash, chain_hash(c.number), "{at}: cursor on the finalized chain");
        assert!(c.number <= rpc.final_head, "{at}: cursor never past the provider");
    }
    if let Some(w) = confirmed(db) {
        assert_eq!(w.hash, chain_hash(w.number), "{at}: watermark on the finalized chain");
        assert!(w.number <= rpc.rpc_final(), "{at}: watermark {} above the RPC head {}", w.number, rpc.rpc_final());
        assert!(w.number >= *floor, "{at}: watermark moved down");
        *floor = w.number;
    }
}

/// Every height from the anchor to the cursor was applied exactly once, on
/// the provider's chain, and none is missing.
fn check_retained(db: &Sqlite, anchor: u64, at: &str) {
    let c = cursor(db).unwrap().number;
    for n in anchor..=c {
        assert_eq!(tx(db, |sql| journal::finalized_hash(sql, n)).unwrap(), Some(chain_hash(n)), "{at}: height {n}");
    }
    let applied = counter(db, "applied_webhook") + counter(db, "applied_rpc");
    assert_eq!(applied, c - anchor, "{at}: each height after the anchor applied once");
}

#[test]
fn deliveries_ahead_of_a_lagging_rpc_never_gap_fault_skip_or_duplicate() {
    for lag in 0..=3 {
        for fork in [false, true] {
            let at = format!("lag {lag}, fork {fork}");
            let db = journal();
            let s = Settings::default();
            let mut rpc = LaggingRpc::new(100, lag);
            rpc.fork_above_final = fork;
            let mut floor = 0;
            let mut now = T0;
            // The first delivery anchors on the RPC head, `lag` blocks below it.
            deliver(&db, &s, &mut rpc, delivery(100, 1, Contract::V2), now);
            check(&db, &rpc, &mut floor, &at);
            let anchor = 100 - lag;
            assert_eq!(confirmed(&db).map(|c| c.number), Some(anchor), "{at}");
            for n in 101..=200 {
                now += BLOCK;
                rpc.final_head = n;
                let (report, _) = deliver(&db, &s, &mut rpc, delivery(n, 1, Contract::V2), now);
                assert_eq!(report.outcome, DeliveryOutcome::Accepted, "{at}: block {n}");
                check(&db, &rpc, &mut floor, &format!("{at}, block {n}"));
                // A gap above the RPC head is never listed: the heights above
                // its finalized head are its best chain only.
                assert!(rpc.max_listed <= rpc.rpc_final(), "{at}: block {n} listed {}", rpc.max_listed);
                if n % 5 == 0 {
                    tick(&db, &s, &mut rpc, now + 6_000);
                    check(&db, &rpc, &mut floor, &format!("{at}, tick after {n}"));
                    // After the first tick, every delivery extends the cursor itself.
                    assert_eq!(cursor(&db).unwrap().number, n, "{at}: tick after {n}");
                }
            }
            // The cursor followed every delivery; RPC confirmed up to its head.
            assert_eq!(cursor(&db).unwrap().number, 200, "{at}");
            // The RPC catches up: the next tick confirms the delivered head.
            rpc.lag = 0;
            tick(&db, &s, &mut rpc, now + 60_000);
            check(&db, &rpc, &mut floor, &at);
            assert_eq!(confirmed(&db).map(|c| c.number), Some(200), "{at}");
            check_retained(&db, anchor, &at);
            assert_eq!(counter(&db, "applied_rpc"), lag.saturating_sub(1), "{at}: only the anchor's gap came by RPC");
            assert_eq!(counter(&db, "deliveries_contract_v2"), 101, "{at}");
            assert_eq!(counter(&db, "rpc_failures"), 0, "{at}");
            // Only the first ticks' head reads and the gap list: about two calls a minute.
            assert!(rpc.calls <= 2 + 21 * 2 + 1, "{at}: {} calls", rpc.calls);
            let st = tx(&db, |sql| status(sql, &s, 3, now + 60_000)).unwrap();
            assert_eq!((st.mode, st.journal.events_conflict), (Mode::Primary, 0), "{at}");
        }
    }
}

#[test]
fn a_retried_delivery_after_the_next_block_under_lag_neither_faults_nor_applies_twice() {
    // Push delivery: block n's first attempt fails, block n + 1 arrives
    // first, and n's retry follows. The gap at n waits for the RPC head (it
    // may be above it) or is filled by the retry; nothing conflicts.
    for lag in 0..=3 {
        for fork in [false, true] {
            let at = format!("lag {lag}, fork {fork}");
            let db = journal();
            let s = Settings::default();
            let mut rpc = LaggingRpc::new(100, lag);
            rpc.fork_above_final = fork;
            let mut floor = 0;
            let mut now = T0;
            deliver(&db, &s, &mut rpc, delivery(100, 1, Contract::V2), now);
            let anchor = 100 - lag;
            assert_eq!(confirmed(&db).map(|c| c.number), Some(anchor), "{at}");
            tick(&db, &s, &mut rpc, now + 6_000);
            for n in (102..=160).step_by(2) {
                // Block n - 1's delivery failed; block n arrives first.
                now += BLOCK;
                rpc.final_head = n;
                deliver(&db, &s, &mut rpc, delivery(n, 1, Contract::V2), now);
                check(&db, &rpc, &mut floor, &format!("{at}, block {n}"));
                // The retry of n - 1, two seconds later (or already filled by RPC).
                let (retry, _) = deliver(&db, &s, &mut rpc, delivery(n - 1, 2, Contract::V2), now + 2_000);
                assert!(
                    matches!(retry.outcome, DeliveryOutcome::Accepted | DeliveryOutcome::DuplicateBlock),
                    "{at}: block {}",
                    n - 1
                );
                check(&db, &rpc, &mut floor, &format!("{at}, retry {}", n - 1));
                assert!(rpc.max_listed <= rpc.rpc_final(), "{at}: block {n} listed {}", rpc.max_listed);
                // The anchor's own gap (lag 2 and 3) closes at the first tick
                // whose head is past it (after block 110); from then on every
                // retry closes its gap itself.
                if n > 110 {
                    assert_eq!(cursor(&db).unwrap().number, n, "{at}: block {n}");
                }
                if n % 10 == 0 {
                    tick(&db, &s, &mut rpc, now + 6_000);
                    check(&db, &rpc, &mut floor, &format!("{at}, tick after {n}"));
                }
            }
            rpc.lag = 0;
            tick(&db, &s, &mut rpc, now + 60_000);
            assert_eq!(confirmed(&db).map(|c| c.number), Some(160), "{at}");
            check_retained(&db, anchor, &at);
        }
    }
}

#[test]
fn a_gap_above_the_rpc_head_waits_for_the_tick_instead_of_reading_its_best_chain() {
    let db = journal();
    let s = Settings::default();
    let mut rpc = LaggingRpc::new(100, 0);
    deliver(&db, &s, &mut rpc, delivery(100, 1, Contract::V2), T0);
    assert_eq!(cursor(&db).map(|c| c.number), Some(100));
    // Blocks 101 and 102 were not delivered (a provider hiccup); 103 is. The
    // endpoint has not finalized past 100 yet, and its best chain above it is
    // another fork. Listing it would apply 101' (its parent is the cursor)
    // and fault on 103's parent.
    rpc.final_head = 103;
    rpc.lag = 3;
    rpc.fork_above_final = true;
    let (report, rounds) = deliver(&db, &s, &mut rpc, delivery(103, 1, Contract::V2), T0 + 3 * BLOCK);
    assert_eq!(report.progress.halt, Some(Halt::Gap { from: 101, to: 102 }));
    assert_eq!((report.plan, rounds, rpc.calls), (None, 0, 2));
    assert_eq!(counter(&db, "gaps_above_rpc_head"), 1);
    // The endpoint finalizes; the next tick reads its head and fills the gap
    // from its finalized chain in the same invocation, then 103 applies and
    // the head audit confirms it.
    rpc.lag = 0;
    tick(&db, &s, &mut rpc, T0 + 60_000);
    assert_eq!(tx(&db, journal::active_fault).unwrap(), None);
    assert_eq!(cursor(&db).map(|c| c.number), Some(103));
    assert_eq!(confirmed(&db).map(|c| (c.number, c.hash)), Some((103, chain_hash(103))));
    check_retained(&db, 100, "gap");
    assert_eq!((counter(&db, "applied_rpc"), counter(&db, "applied_webhook")), (2, 1));
}

#[test]
fn a_null_header_from_a_lagging_node_is_retried_at_the_next_delivery_or_tick() {
    let db = journal();
    let s = Settings::default();
    let mut rpc = LaggingRpc::new(100, 2);
    // The anchor's header reads hit a node that does not serve the head yet:
    // the delivery's two rounds (a bounded retry) both answer `null`.
    rpc.null_headers = 2;
    let (first, rounds) = deliver(&db, &s, &mut rpc, delivery(100, 1, Contract::V2), T0);
    assert!(matches!(first.plan, Some(RpcPlan::FinalizedHead { purpose: HeadPurpose::Anchor, .. })));
    assert_eq!(rounds, s.delivery_rounds);
    assert_eq!((cursor(&db), counter(&db, "rpc_not_served")), (None, 2));
    // The claim was released: the next delivery anchors on the RPC head (99)
    // and both pending deliveries apply on top of it.
    rpc.final_head = 101;
    deliver(&db, &s, &mut rpc, delivery(101, 1, Contract::V2), T0 + BLOCK);
    assert_eq!(cursor(&db).map(|c| c.number), Some(101));
    assert_eq!(confirmed(&db).map(|c| c.number), Some(99));
    // A tick's head read answered `null` changes nothing (no mode change, no
    // confirmation, no fault); the next tick reads it.
    rpc.null_headers = 1;
    let before = confirmed(&db);
    let report = tick(&db, &s, &mut rpc, T0 + 60_000);
    assert!(!report.fallback);
    assert_eq!((confirmed(&db), counter(&db, "rpc_not_served")), (before, 3));
    assert_eq!(tx(&db, journal::active_fault).unwrap(), None);
    tick(&db, &s, &mut rpc, T0 + 120_000);
    assert_eq!(confirmed(&db).map(|c| c.number), Some(99));
    // Once the RPC finalizes 101, the next tick confirms it.
    rpc.lag = 0;
    tick(&db, &s, &mut rpc, T0 + 180_000);
    assert_eq!(cursor(&db).map(|c| c.number), Some(101));
    assert_eq!(confirmed(&db).map(|c| c.number), Some(101));
    let st = tx(&db, |sql| status(sql, &s, 3, T0 + 180_000)).unwrap();
    assert_eq!(st.recent_transitions.len(), 0, "no mode change");
}

#[test]
fn a_hash_list_cut_short_by_a_lagging_node_defers_instead_of_repeating() {
    let db = journal();
    let s = Settings::default();
    let mut rpc = LaggingRpc::new(100, 0);
    deliver(&db, &s, &mut rpc, delivery(100, 1, Contract::V2), T0);
    // The provider stalled for a few blocks; the tick reads the RPC head 112.
    rpc.final_head = 112;
    tick(&db, &s, &mut rpc, T0 + 60_000);
    assert_eq!(cursor(&db).map(|c| c.number), Some(100));
    // Deliveries resume at 113, but the node answering the list serves only
    // up to 105 (a lagging node behind the endpoint's load balancer).
    rpc.final_head = 113;
    rpc.lists_serve_to = Some(105);
    rpc.calls = 0;
    let (_, rounds) = deliver(&db, &s, &mut rpc, delivery(113, 1, Contract::V2), T0 + 61_000);
    // One list (100..=112): 101..105 recorded, then it stops; no second,
    // identical call in this invocation.
    assert_eq!((rounds, rpc.calls), (1, 1));
    assert_eq!(cursor(&db).map(|c| c.number), Some(105));
    assert_eq!(counter(&db, "rpc_lists_not_served"), 1);
    // The next delivery continues where it stopped once the node serves.
    rpc.lists_serve_to = None;
    rpc.final_head = 114;
    deliver(&db, &s, &mut rpc, delivery(114, 1, Contract::V2), T0 + 61_000 + BLOCK);
    assert_eq!(cursor(&db).map(|c| c.number), Some(114));
    assert_eq!(tx(&db, journal::active_fault).unwrap(), None);
    check_retained(&db, 100, "short list");
}

#[test]
fn contract_one_and_two_deliveries_drive_the_same_path_and_are_counted_apart() {
    let db = journal();
    let s = Settings::default();
    let mut rpc = LaggingRpc::new(100, 0);
    let contracts = [Contract::Unnamed, Contract::V1, Contract::V2];
    deliver(&db, &s, &mut rpc, delivery(100, 1, Contract::Unnamed), T0);
    for n in 101..=130u64 {
        rpc.final_head = n;
        // Under contract 1 the RPC already serves the block; under 2 it may not.
        rpc.lag = if n % 2 == 0 { 0 } else { 2 };
        let c = contracts[(n % 3) as usize];
        let (r, _) = deliver(&db, &s, &mut rpc, delivery(n, 1, c), T0 + (n - 100) * BLOCK);
        assert_eq!((r.outcome, r.progress.applied, r.plan), (DeliveryOutcome::Accepted, 1, None), "block {n}");
        // The provider's retry of the same delivery is a duplicate, counted once.
        let (dup, _) = deliver(&db, &s, &mut rpc, delivery(n, 2, c), T0 + (n - 100) * BLOCK + 2_000);
        assert_eq!(dup.outcome, DeliveryOutcome::DuplicateDelivery);
    }
    // 100 (unnamed) plus 30 blocks split evenly by n % 3.
    assert_eq!(counter(&db, "deliveries_contract_unnamed"), 11);
    assert_eq!(counter(&db, "deliveries_contract_v1"), 10);
    assert_eq!(counter(&db, "deliveries_contract_v2"), 10);
    assert_eq!(counter(&db, "duplicate_deliveries"), 30);
    assert_eq!(counter(&db, "applied_webhook"), 30);
    assert_eq!(rpc.calls, 2, "the anchor only");
}
