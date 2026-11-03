//! Native witness tests for the arena routes (`--features arena-witness`, one
//! test thread: `cargo test --features arena-witness arena_witness --
//! --test-threads=1`).
//!
//! The arena is this test binary's global allocator (lib.rs). Each witness
//! runs a route's heaviest native path under `Mode::Strict` inside a scope
//! with exactly the route's declared control footprint (`arena_routes`), and
//! fails on any allocation outside the scope, any overdraft past the
//! footprint and anything left live at the end. Native sizes are 64-bit, so
//! they bound the wasm32 sizes of the same structures from above.
//!
//! The measurements (`home_caches_fit`) print the peak pages they found, the
//! inputs of `arena_routes::RUNTIME_PARSED_BYTES` and `PLAN_PARSED_BYTES`.

use crate::arena_routes::{self, *};
use crate::eval::engine::{self, EvalConfig, Kick};
use crate::eval::plan::{Ceilings, CohortPlan};
use crate::sql::{self, CHAIN_JOURNAL_SCHEMA, EVAL_COHORT_SCHEMA};
use crate::testing::{self, Sqlite, tx};
use crate::{heartbeat, webhook};
use slop_ninja_arena::{Closed, Mode, PAGE, Route, Scope, admit_current_block, set_mode, stats, strict};
use slop_ninja_protocol::{ChainBinding, TESTNET_GENESIS};

const MEASURE: Route = Route::new(200, "witness/measure");
const METADATA: &[u8] = include_bytes!("../../fixtures/runtime-486/testnet-486.v15.scale");

/// Peak pages (state page included) and peak live bytes of `f` run in a
/// fresh scope. `f` drops whatever it allocates before returning.
fn measure(f: impl FnOnce()) -> (u32, usize) {
    let s = Scope::open(MEASURE, 0).expect("measure scope");
    s.enter(f);
    let st = s.stats();
    assert!(matches!(s.close(), Closed::Released { .. }), "the measured closure left nothing live");
    (st.peak_pages, st.peak_live)
}

/// Runs `f` under `Strict` in a scope of exactly `control` bytes and fails
/// the test on any violation; prints the scope's peak pages (state page
/// included), the measured input of the route's footprint.
fn witness<R>(route: Route, control: usize, f: impl FnOnce() -> R) -> R {
    let prev = set_mode(Mode::Strict);
    let _ = strict::take();
    let armed = strict::arm();
    let scope = Scope::open(route, control).unwrap_or_else(|r| panic!("route {} admitted: {r:?}", route.name));
    let out = scope.enter(f);
    let st = scope.stats();
    let closed = scope.close();
    drop(armed);
    set_mode(prev);
    let violations = strict::take();
    eprintln!(
        "arena witness: route {} peak {} pages of {} reserved ({} B footprint), peak live {} B",
        route.name, st.peak_pages, st.reserved_pages, control, st.peak_live
    );
    assert!(violations.is_clean(), "route {} broke its footprint of {control} B: {violations:?}", route.name);
    assert!(matches!(closed, Closed::Released { .. }), "route {} left values live: {closed:?}", route.name);
    assert!(st.peak_pages <= st.reserved_pages, "route {} used {} pages of {}", route.name, st.peak_pages, st.reserved_pages);
    out
}

/// The main instance without a roster cap (mainnet's configuration), so the
/// widest plan the body bound allows is admitted.
fn cfg() -> EvalConfig {
    EvalConfig {
        chain: ChainBinding { genesis: TESTNET_GENESIS, netuid: 601, mechanism: 0 },
        private_seed: [5; 32],
        instance: engine::INSTANCE_MAIN.into(),
        max_score_lanes: 64,
        ceilings: Ceilings { roster_cap: None, accept_reward_v0: true },
        exposed_pools: vec!["p".into()],
        scorer_binary: String::new(),
        authority: crate::config::Authority::default(),
    }
}

fn plan_with(entrants: u16) -> (CohortPlan, Vec<u8>) {
    use crate::eval::plan::tests::{artifact, identity, plan};
    let mut p = plan(1000);
    p.entrants = (1..=entrants).map(|u| artifact(identity(u, u as u8), 0x22)).collect();
    p.allow_list = (1..=entrants).map(|u| identity(u, u as u8)).collect();
    let json = serde_json::to_vec(&p).expect("plan json");
    (p, json)
}

/// The largest entrant count for which `ok` holds (it holds at 1 and is
/// monotone in the count).
fn largest(ok: impl Fn(u16) -> bool) -> u16 {
    let (mut lo, mut hi) = (1u16, 4096u16);
    while lo < hi {
        let mid = lo + (hi - lo).div_ceil(2);
        if ok(mid) { lo = mid } else { hi = mid - 1 }
    }
    lo
}

/// The largest plan body `/plan` accepts (`MAX_PLAN_BODY_BYTES`): the
/// largest roster whose JSON fits it.
fn widest_plan() -> (CohortPlan, Vec<u8>) {
    plan_with(largest(|e| plan_with(e).1.len() <= MAX_PLAN_BODY_BYTES))
}

/// The widest plan the main instance admits (receipt-index and budget
/// bounds included) within the body bound.
fn widest_admitted() -> (CohortPlan, Vec<u8>) {
    let main = cfg();
    plan_with(largest(|e| {
        let (_, json) = plan_with(e);
        json.len() <= MAX_PLAN_BODY_BYTES && engine::admit(&main, &json).is_ok()
    }))
}

#[test]
fn arena_witness_home_caches_fit() {
    set_mode(Mode::Enforce);
    // A parsed runtime, in the home scope: parse transients included.
    let (rt_pages, rt_live) = measure(|| {
        let rt = crate::chain::ChainRuntime::parse(METADATA, 486, 1).expect("fixture metadata parses");
        drop(rt);
    });
    // A cached plan: the clone the home scope keeps of the widest plan.
    let (plan, json) = widest_plan();
    let (plan_pages, plan_live) = measure(|| {
        let copy = plan.clone();
        let key = (1i64, "0".repeat(64));
        drop((key, copy));
    });
    eprintln!(
        "arena witness: parsed runtime peak {rt_pages} pages ({} B), live {rt_live} B; plan clone of a {} B plan: peak {plan_pages} pages ({} B), live {plan_live} B",
        rt_pages as usize * PAGE,
        json.len(),
        plan_pages as usize * PAGE
    );
    // The measured peaks include the state page; the constants are heap
    // footprints for one entry each.
    assert!(rt_pages as usize * PAGE <= RUNTIME_PARSED_BYTES, "RUNTIME_PARSED_BYTES covers a parsed runtime");
    assert!(plan_pages as usize * PAGE <= PLAN_PARSED_BYTES, "PLAN_PARSED_BYTES covers a cached plan");
    // Each cache at its entry bound stays inside its home's footprint.
    witness(R_HOME_RUNTIMES, HOME_RUNTIMES_BYTES, || {
        let mut runtimes = Vec::with_capacity(crate::signing::PARSED_RUNTIMES);
        for _ in 0..crate::signing::PARSED_RUNTIMES {
            runtimes.push(crate::chain::ChainRuntime::parse(METADATA, 486, 1).expect("parse"));
        }
        drop(runtimes);
    });
    witness(R_HOME_PLANS, HOME_PLANS_BYTES, || {
        let mut plans = Vec::with_capacity(engine::PLAN_CACHE_PLANS);
        for seq in 0..engine::PLAN_CACHE_PLANS {
            plans.push(((seq as i64, "0".repeat(64)), plan.clone()));
        }
        drop(plans);
    });
}

#[test]
fn arena_witness_a_delivery_fits_the_hook_footprint() {
    set_mode(Mode::Enforce);
    let receiver = testing::receiver();
    let raw = testing::finalized_body(7, 1_000, 0x31, 0x30);
    let now = 1_793_000_000;
    let header = testing::sign(testing::context(7, 9), 1, now, &raw);
    let settings = heartbeat::Settings::default();
    let db = Sqlite::open();
    tx(&db, |s| sql::migrate(s, &CHAIN_JOURNAL_SCHEMA)).expect("migrate");
    // The Worker side: the body is an admitted block of its exact length.
    let body = witness(R_HOOK.route(), HOOK_CONTROL_BYTES, || {
        assert_eq!(admit_current_block(raw.len(), 1), Ok(true));
        let mut copy = Vec::with_capacity(raw.len());
        copy.extend_from_slice(&raw);
        let signed = webhook::authenticate(&receiver, Some(&header), &copy, now).expect("authentic");
        let webhook::Delivery::Finalized(n) = webhook::parse_authenticated(&receiver, &signed, &copy).expect("parses") else {
            panic!("finalized")
        };
        serde_json::to_string(&n).expect("json").len()
    });
    assert!(body > 0);
    // The journal side: ingest and the status read.
    let n = {
        let signed = webhook::authenticate(&receiver, Some(&header), &raw, now).unwrap();
        match webhook::parse_authenticated(&receiver, &signed, &raw).unwrap() {
            webhook::Delivery::Finalized(n) => n,
            _ => unreachable!(),
        }
    };
    witness(R_JOURNAL_FETCH.route(), JOURNAL_CONTROL_BYTES, || {
        let report = tx(&db, |s| heartbeat::on_delivery(s, &settings, &n, now * 1000)).expect("ingest");
        serde_json::to_vec(&report).expect("json").len()
    });
    witness(R_JOURNAL_FETCH.route(), JOURNAL_CONTROL_BYTES, || {
        let report = tx(&db, |s| heartbeat::on_tick(s, &settings, now * 1000 + 60_000)).expect("tick");
        let status = tx(&db, |s| heartbeat::status(s, &settings, 1, now * 1000 + 60_000)).expect("status");
        serde_json::to_vec(&(report, status)).expect("json").len()
    });
}

#[test]
fn arena_witness_the_stored_runtime_read_fits_the_journal_footprint() {
    set_mode(Mode::Enforce);
    // What a signing step reads in its own scope before the home scope
    // parses: the stored metadata row (hex text) and its decoded bytes.
    let hex_text = hex::encode(METADATA);
    witness(R_JOURNAL_FETCH.route(), JOURNAL_CONTROL_BYTES, || {
        let row = hex_text.clone();
        let bytes = hex::decode(&row).expect("hex");
        let digest = crate::chain::sha256_hex(&bytes);
        (bytes.len(), digest.len())
    });
}

/// A chain upgrade with a full runtime cache: the third runtime evicts
/// before it parses, so the runtimes home never holds more than its
/// footprint (one parse beside `PARSED_RUNTIMES - 1` residents).
#[test]
fn arena_witness_a_third_runtime_loads_into_a_full_cache_within_the_home() {
    use crate::sql::SqlValue;
    set_mode(Mode::Enforce);
    let db = Sqlite::open();
    tx(&db, |s| sql::migrate(s, &CHAIN_JOURNAL_SCHEMA)).expect("migrate");
    let hex_text = hex::encode(METADATA);
    let digest = crate::chain::sha256_hex(METADATA);
    let specs = [486u32, 487, 488, 489];
    for (i, spec) in specs.iter().enumerate() {
        tx(&db, |s| {
            crate::sql::Sql::exec(
                s,
                "INSERT INTO runtime_cache (spec_version, tx_version, metadata_sha256, metadata_hex, block_number, block_hash, fetched_at_ms)
                 VALUES (?, 1, ?, ?, ?, ?, ?)",
                &[
                    SqlValue::Integer(i64::from(*spec)),
                    SqlValue::Text(digest.clone()),
                    SqlValue::Text(hex_text.clone()),
                    SqlValue::Integer(i as i64),
                    SqlValue::Text(format!("0x{:064x}", i)),
                    SqlValue::Integer(i as i64),
                ],
            )
        })
        .expect("stored runtime");
    }
    let cache = crate::signing::RuntimeCache::default();
    for spec in specs {
        witness(R_JOURNAL_FETCH.route(), JOURNAL_CONTROL_BYTES, || {
            let rt = tx(&db, |s| cache.load(s, spec, 1)).expect("load").expect("stored");
            drop(rt);
        });
    }
    let home = arena_routes::home::stats(arena_routes::home::Home::Runtimes).expect("the cache opened the runtimes home");
    eprintln!("arena witness: home/runtimes after {} loads: peak {} of {} pages, overdraft {}", specs.len(), home.peak_pages, home.reserved_pages, home.overdraft_pages);
    assert!(home.peak_pages <= home.reserved_pages && home.overdraft_pages == 0, "the runtimes home stayed inside its footprint");
    cache.forget();
}

#[test]
fn arena_witness_cohort_routes_fit_their_footprints() {
    set_mode(Mode::Enforce);
    let (plan, json) = widest_admitted();
    eprintln!("arena witness: widest admitted plan {} B, {} entrants", json.len(), plan.entrants.len());
    assert!(json.len() <= MAX_PLAN_BODY_BYTES);
    let db = Sqlite::open();
    tx(&db, |s| sql::migrate(s, &EVAL_COHORT_SCHEMA)).expect("migrate");
    let main = cfg();
    // `/plan`: the admitted body block, the admission parse and the queue.
    let queued = witness(R_COHORT_FETCH.route(), COHORT_CONTROL_BYTES, || {
        assert_eq!(admit_current_block(json.len(), 1), Ok(true));
        let mut body = Vec::with_capacity(json.len());
        body.extend_from_slice(&json);
        let (_, adm) = engine::admit(&main, &body).expect("admitted");
        let seq = tx(&db, |s| engine::queue(s, &main, &body, 1_000)).expect("queued");
        (seq, adm.reserve_microusd)
    });
    assert!(queued.0 >= 0);
    // `/kick` and `/status` over the queued widest plan (the plan copy).
    witness(R_COHORT_FETCH.route(), COHORT_CONTROL_BYTES, || {
        let moved = tx(&db, |s| engine::observe(s, &Kick { confirmed: None, epoch: None }, 2_000)).expect("kick");
        moved.len()
    });
    witness(R_COHORT_FETCH.route(), COHORT_CONTROL_BYTES, || {
        let v = tx(&db, |s| engine::status(s, 3_000)).expect("status");
        serde_json::to_vec(&v).expect("json").len()
    });
    // The alarm's own work outside its seal and package steps: picking the
    // next work item over the queued widest plan.
    witness(R_COHORT_ALARM.route(), ALARM_CONTROL_BYTES, || {
        let work = tx(&db, |s| engine::next_work(s, &main, 4_000)).expect("next work");
        work.is_some()
    });
    drop(plan);
    // The plans home holds the cache's copies (not the request's scope).
    let home = arena_routes::home::stats(arena_routes::home::Home::Plans).expect("the plan cache opened the plans home");
    assert!(home.live > 0, "the cached plan lives in the plans home");
}

/// A plan naming spec v3 with its frozen digest: `/plan` validation
/// rebuilds the spec record once to compare the digest and fits the route's
/// footprint; what a scoring lane's report reads per call (the plan's frozen
/// digest and the spec-derived size bound) never rebuilds it, so it stays far
/// below the spec record's size.
#[test]
fn arena_witness_a_plan_naming_spec_v3_fits_and_lane_reports_never_rebuild_the_spec() {
    set_mode(Mode::Enforce);
    let v3 = slop_ninja_artifact_check::spec::Version::V3;
    let (mut plan, _) = plan_with(1);
    let s = &mut plan.fanout.as_mut().expect("fanout").score;
    s.artifact_max_mib = None;
    s.artifact_spec = Some(v3.id().into());
    s.load_ms_per_mib = Some(50);
    s.artifact_spec_sha256 = Some(v3.spec_sha256());
    let json = serde_json::to_vec(&plan).expect("plan json");
    let db = Sqlite::open();
    tx(&db, |s| sql::migrate(s, &EVAL_COHORT_SCHEMA)).expect("migrate");
    let main = cfg();
    witness(R_COHORT_FETCH.route(), COHORT_CONTROL_BYTES, || {
        assert_eq!(admit_current_block(json.len(), 1), Ok(true));
        let mut body = Vec::with_capacity(json.len());
        body.extend_from_slice(&json);
        let _ = engine::admit(&main, &body).expect("admitted");
        tx(&db, |s| engine::queue(s, &main, &body, 1_000)).expect("queued")
    });
    let (_, rebuild_live) = measure(|| {
        let _ = crate::eval::plan::load_spec_sha256_of(v3.id());
    });
    let (_, report_live) = measure(|| {
        let limits = plan.fanout().expect("fanout").score.limits();
        assert!(limits.artifact_max_bytes < u64::MAX);
        assert_eq!(plan.load_spec_sha256().map(str::len), Some(64));
    });
    eprintln!("arena witness: spec v3 digest rebuild peak live {rebuild_live} B, lane report reads {report_live} B");
    assert!(rebuild_live > 0);
    assert!(report_live * 16 < rebuild_live, "a lane report rebuilds the spec record");
}

#[test]
fn arena_witness_no_unscoped_allocation_reaches_glue_inside_a_route() {
    set_mode(Mode::Enforce);
    let before = stats();
    witness(R_STATUS.route(), WORKER_CONTROL_BYTES, || {
        let v = vec![0u8; 4096];
        v.len()
    });
    let after = stats();
    assert_eq!(after.armed_unscoped, before.armed_unscoped, "nothing in the route allocated outside its scope");
}

#[test]
fn arena_witness_a_refused_home_opens_at_a_later_use() {
    // A home refused while the pool had no room (a package step holding it)
    // is not disabled for the instance's life: the next use opens it.
    use arena_routes::home::{self, Home};
    set_mode(Mode::Enforce);
    let h = Home::State;
    assert!(home::stats(h).is_none(), "no test opened the state home before this one");
    let limit = slop_ninja_arena::page_limit();
    let failures = home::open_failures(h);
    // Leave one free page: the home (state page and its run) cannot open.
    slop_ninja_arena::shrink_free_to(1).expect("shrink");
    let refused = home::enter(h, || 1u32).is_err();
    slop_ninja_arena::set_page_limit(limit).expect("restore the pool");
    assert!(refused, "the home was refused while the pool had no room");
    assert_eq!(home::open_failures(h), failures + 1, "the refusal is counted");
    assert_eq!(home::enter(h, || 2u32).ok(), Some(2), "the next use opens the home");
    assert!(home::stats(h).is_some(), "the home stays open");
}
