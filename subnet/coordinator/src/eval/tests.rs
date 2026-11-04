//! Whole cohorts, natively, against SQLite, with every bulk step running as
//! fan-out lanes in a simulated lane runtime: a queue carrying content-free
//! kicks, lane Durable Objects that order assignments by `(seq, gen)`, alarm
//! loops, and the two buckets. The engine functions are the real ones.
//!
//! Covered: a cohort end to end through generation, scoring, summary and bundle
//! lanes and packaging, with every bundled receipt verified; lease expiry and
//! reconciliation (with and without durable objects); a lane crash and its
//! re-kick; a container crash or OOM and the crash-isolation rule; isolation
//! cut short by the step budget and continued; one-off failures; artifacts
//! that crash on everything or never load; R2 read outages before dispatch and
//! during reconciliation; duplicate and missing wakeups; the re-kick livelock
//! regression; coordinator silence and resume; generation throttling, budgets
//! and fences; job-scoped faults; the scoring budget; load mode; and the store
//! migrations. Scoring lanes run the Worker's own `lane_logic::run_chunk`.

use super::chain::EpochObservation;
use super::engine::{self, EvalConfig, GenOutcome, Kick, Work};
use super::lane_logic::{self, ChunkCtx, ChunkEnd, CohortError, Fault, LaneIo, RestartFailure, ScoreFailure};
use super::lanes::{self, Assignment, ChunkClaim, ClaimReply, GenClaimReply, LaneKick, LaneRef, LaneReport};
use super::plan::tests::plan;
use super::plan::{CohortPlan, Ceilings};
use super::pool::{self, PoolManifest, PoolRow, ShardInfo};
use super::receipts;
use super::textgen::{self, Reject};
use super::v3;
use crate::sql::{self, EVAL_COHORT_SCHEMA};
use crate::testing::{Sqlite, tx};
use crate::wire::{BlockRef, Id};
use serde_json::{Value, json};
use sha2::{Digest as _, Sha256};
use slop_ninja_protocol::{
    AccountId, ChainBinding, NeuronIdentity, RegistrationSnapshot, Record, TESTNET_GENESIS, reward,
};
use std::collections::{BTreeMap, BTreeSet};

fn cfg() -> EvalConfig {
    EvalConfig {
        chain: ChainBinding {
            genesis: TESTNET_GENESIS,
            netuid: 601,
            mechanism: 0,
        },
        private_seed: [5; 32],
        instance: engine::INSTANCE_MAIN.into(),
        max_score_lanes: 64,
        ceilings: Ceilings { roster_cap: Some(40), accept_reward_v0: true },
        exposed_pools: vec!["p".into()],
        scorer_binary: String::new(),
        authority: crate::config::Authority::default(),
    }
}

fn load_cfg() -> EvalConfig {
    EvalConfig { instance: engine::INSTANCE_LOAD.into(), ceilings: Ceilings { roster_cap: None, accept_reward_v0: true }, ..cfg() }
}

fn kick_at(db: &Sqlite, index: u64, now: u64) -> Vec<String> {
    let block = BlockRef {
        number: 1000 + index * 361,
        hash: Id([index as u8; 32]),
    };
    let k = Kick {
        confirmed: Some(block),
        epoch: Some(EpochObservation {
            block,
            index,
            last_epoch_block: 990 + index * 361,
        }),
        // A live observation: the watermark is the head.
        head_epoch: Some(index),
    };
    tx(db, |s| engine::observe(s, &k, now)).unwrap()
}

/// A tick during a catch-up: the epoch read at a lagging confirmed watermark
/// `block` (in epoch `index`) while the head is in epoch `head`.
fn kick_lagging(db: &Sqlite, block: u64, index: u64, head: u64, now: u64) -> Vec<String> {
    let at = BlockRef { number: block, hash: Id([(block % 251) as u8; 32]) };
    let k = Kick {
        confirmed: Some(at),
        epoch: Some(EpochObservation { block: at, index, last_epoch_block: 990 + index * 361 }),
        head_epoch: Some(head),
    };
    tx(db, |s| engine::observe(s, &k, now)).unwrap()
}

/// A heartbeat with no new epoch (a webhook delivery).
fn heartbeat(db: &Sqlite, now: u64) {
    tx(db, |s| engine::observe(s, &Kick::default(), now)).unwrap();
}

fn human_text(g: usize, i: usize) -> String {
    format!(
        "Group {g} item {i} opens with a plain first sentence about local weather. \
The second sentence adds a detail about the harbour and the boats moored there. \
A third sentence mentions the market that opens on Saturdays. \
Finally the fourth sentence closes the paragraph with a note on the evening tide."
    )
}

fn pool_fixture() -> (PoolManifest, BTreeMap<String, String>) {
    pool_of(40)
}

/// `groups` source groups of one or two humans each (1.5 per group on average).
fn pool_of(groups: usize) -> (PoolManifest, BTreeMap<String, String>) {
    let mut rows = Vec::new();
    let mut texts = BTreeMap::new();
    for g in 0..groups {
        let domain = if g % 3 == 0 { "wikinews" } else { "wikipedia" };
        for i in 0..(1 + g % 2) {
            let t = human_text(g, i);
            let id = format!("row-{g}-{i}");
            rows.push(PoolRow {
                row_id: id.clone(),
                text_sha256: hex::encode(Sha256::digest(t.as_bytes())),
                domain: domain.into(),
                source_group: format!("group-{g}"),
                shard: 0,
            });
            texts.insert(id, t);
        }
    }
    let m = PoolManifest {
        schema: pool::POOL_SCHEMA.into(),
        pool_id: "p".into(),
        shards: vec![ShardInfo {
            index: 0,
            file: "shard-0000.jsonl".into(),
            rows: rows.len() as u32,
            sha256: String::new(),
        }],
        rows,
    };
    (m, texts)
}

/// A model that rewrites by reversing word order and writes scratch text of
/// the right length.
fn answer(it: &engine::GenItem) -> String {
    if it.kind == "mixed" {
        let src = it.prompt.user.split("\n\n").nth(1).unwrap().to_owned();
        let mut w: Vec<&str> = src.split_whitespace().collect();
        w.reverse();
        w.join(" ")
    } else {
        let n: usize = it.prompt.user.split("about ").nth(2).unwrap().split(' ').next().unwrap().parse().unwrap();
        vec!["prose"; n].join(" ")
    }
}

fn outcome(it: &engine::GenItem, output: Result<String, Reject>) -> GenOutcome {
    GenOutcome {
        idx: it.idx,
        generator: it.generator.clone(),
        output,
        prompt_tokens: 300,
        completion_tokens: 200,
        ms: 900,
        claim: it.claim,
    }
}

/// Scorer stand-in: involvement by kind, identical for every artifact.
fn fake_results(db: &Sqlite, seq: i64, docs: &[(u32, String, String)]) -> Vec<Value> {
    docs.iter()
        .map(|(task, _, sha)| {
            let kind: String = if *task == lane_logic::CONTROL_TASK {
                "control".into()
            } else {
                db.conn
                    .query_row("SELECT kind FROM docs WHERE seq = ?1 AND task = ?2", rusqlite::params![seq, task], |r| r.get(0))
                    .unwrap()
            };
            let (c, s) = match kind.as_str() {
                "human_only" => (12.5, 3.0),
                "mixed" => (88.0, 40.0),
                _ => (97.0, 93.0),
            };
            json!({"id": task.to_string(), "status": "ok", "text_sha256": sha, "token_count": 120,
                "prediction": {"ai_confidence_percent": c, "ai_share_percent": s,
                    "attribution": {"human": {"profile": "other_human"}, "ai": {"profile": "other_ai"}}}})
        })
        .collect()
}

/// Scorer stand-in for reward-v1 tests: predictions from the sim's hook.
fn predicted(db: &Sqlite, seq: i64, job: i64, docs: &[(u32, String, String)], p: &dyn Fn(i64, &str, u32) -> (f64, f64)) -> Vec<Value> {
    docs.iter()
        .map(|(task, _, sha)| {
            let kind: String = if *task == lane_logic::CONTROL_TASK {
                "control".into()
            } else {
                db.conn
                    .query_row("SELECT kind FROM docs WHERE seq = ?1 AND task = ?2", rusqlite::params![seq, task], |r| r.get(0))
                    .unwrap()
            };
            let (c, s) = p(job, &kind, *task);
            json!({"id": task.to_string(), "status": "ok", "text_sha256": sha, "token_count": 120,
                "prediction": {"ai_confidence_percent": c, "ai_share_percent": s.min(c),
                    "attribution": {"human": {"profile": "other_human"}, "ai": {"profile": "other_ai"}}}})
        })
        .collect()
}

// --------------------------------------------------------------- the runtime

/// What the simulated scorer does with one request.
#[derive(Clone, Debug, PartialEq)]
enum Score {
    Ok,
    /// Dispatched, then the container crashed (transport error, OOM).
    Crash,
    /// Dispatched, and failed with the container still up and ready.
    Fail,
    /// The scorer refused before inference.
    NotDispatched,
    /// The lane's invocation dies after its claim, before any put.
    DieBeforePut,
    /// The lane dies after both puts, before its commit.
    DieAfterPut,
}

/// One scorer request as the hook sees it.
#[allow(dead_code)]
struct Req {
    lane: String,
    job: i64,
    chunk: i64,
    attempt: u8,
    tasks: Vec<u32>,
    isolate: bool,
}

#[derive(Clone, Debug)]
struct LaneDo {
    seq: i64,
    kind: String,
    lane: i64,
    generation: i64,
    armed: bool,
}

type ScoreHook<'a> = Box<dyn FnMut(&Req) -> Score + 'a>;
/// A lane's prepare or restart: the job and whether it is an isolation restart.
type StartHook<'a> = Box<dyn FnMut(i64, bool) -> Result<(), RestartFailure> + 'a>;
/// Whether an R2 operation on a key fails (`true` for a put).
type R2Hook<'a> = Box<dyn FnMut(&str, bool) -> bool + 'a>;
type GenHook<'a> = Box<dyn FnMut(&engine::GenItem) -> Result<String, Reject> + 'a>;
/// reward-v1 scorer stand-in: `(job, kind, task) -> (confidence %, share %)`.
type PredictHook<'a> = Box<dyn Fn(i64, &str, u32) -> (f64, f64) + 'a>;

struct Sim<'a> {
    db: &'a Sqlite,
    cfg: EvalConfig,
    now: u64,
    pool: PoolManifest,
    texts: BTreeMap<String, String>,
    eval: BTreeMap<String, Vec<u8>>,
    rcpt: BTreeMap<String, Vec<u8>>,
    queue: Vec<LaneKick>,
    lanes: BTreeMap<String, LaneDo>,
    score: ScoreHook<'a>,
    generate: GenHook<'a>,
    /// Kicks the queue loses (every `n`th), and whether it delivers twice.
    drop_every: Option<usize>,
    duplicate: bool,
    sent: usize,
    delivered: usize,
    /// Heartbeats arrive with every tick.
    heartbeats: bool,
    packages: Vec<engine::PackageBuild>,
    tick_ms: u64,
    calls: u32,
    /// Claims a lane makes per alarm invocation (its step budget), and the
    /// rate-limiter grant per model per claim.
    claims_per_run: usize,
    grant: u32,
    /// Lane kinds whose alarms do not run (to inspect dispatch alone).
    frozen: Vec<&'static str>,
    start: StartHook<'a>,
    r2: R2Hook<'a>,
    /// Lanes whose container holds the artifact loaded.
    loaded: BTreeSet<String>,
    /// Virtual time a lane invocation spends per request and per restart,
    /// against the plan's step budget.
    doc_ms: u64,
    restart_ms: u64,
    elapsed: u64,
    /// Restarts and isolation hand-backs seen, for assertions.
    restarts: u32,
    /// reward-v1: per-job predictions `(job, kind, task) -> (confidence %, share %)`.
    predict: Option<PredictHook<'a>>,
    packages_v3: Vec<v3::PackageV3>,
    closures: Vec<v3::ClosureBuild>,
    /// Outage probes run (`engine::probe_due`), by fault code.
    probes: Vec<String>,
    /// The job whose artifact each scorer probe started.
    probe_jobs: Vec<i64>,
    /// Virtual time each probe takes (the clock moves on by it).
    probe_ms: u64,
    /// The cohort and start time of each probe.
    probe_starts: Vec<(i64, u64)>,
    /// The spend ledger does not answer: every settle fails.
    ledger_down: bool,
    /// Settles attempted, for assertions.
    settle_calls: u32,
}

impl<'a> Sim<'a> {
    fn new(db: &'a Sqlite, cfg: EvalConfig, now: u64) -> Self {
        let (pool, texts) = pool_fixture();
        Self {
            db,
            cfg,
            now,
            pool,
            texts,
            eval: BTreeMap::new(),
            rcpt: BTreeMap::new(),
            queue: Vec::new(),
            lanes: BTreeMap::new(),
            score: Box::new(|_| Score::Ok),
            generate: Box::new(|it| Ok(answer(it))),
            drop_every: None,
            duplicate: false,
            sent: 0,
            delivered: 0,
            heartbeats: true,
            packages: Vec::new(),
            tick_ms: 1000,
            calls: 0,
            claims_per_run: 500,
            grant: 4,
            frozen: Vec::new(),
            start: Box::new(|_, _| Ok(())),
            r2: Box::new(|_, _| false),
            loaded: BTreeSet::new(),
            doc_ms: 1_000,
            restart_ms: 30_000,
            elapsed: 0,
            restarts: 0,
            predict: None,
            packages_v3: Vec::new(),
            closures: Vec::new(),
            probes: Vec::new(),
            probe_jobs: Vec::new(),
            probe_ms: 0,
            probe_starts: Vec::new(),
            ledger_down: false,
            settle_calls: 0,
        }
    }

    /// One cohort alarm step: root steps until a sweep, then the sweep's kicks.
    fn step(&mut self) -> bool {
        let c = self.cfg.clone();
        let mut progress = false;
        for _ in 0..8 {
            let now = self.now;
            let Some(w) = tx(self.db, |s| engine::next_work(s, &c, now)).unwrap() else { break };
            progress = true;
            let is_v3 = |seq: i64| tx(self.db, |s| Ok::<_, engine::EvalError>(engine::cohort(s, seq)?.plan.is_v3())).unwrap();
            match w {
                Work::Draw { seq, plan } if plan.is_v3() => self.draw_v3(seq, &plan),
                Work::Seal { seq } if is_v3(seq) => self.seal_v3(seq),
                Work::Package { seq } if is_v3(seq) => self.package_v3(seq),
                Work::Close { seq } => {
                    let b = tx(self.db, |s| v3::prepare_closure(s, &c, seq)).unwrap();
                    let digest = b.closure.digest().unwrap();
                    tx(self.db, |s| v3::commit_closure(s, seq, &digest, 4)).unwrap();
                    self.closures.push(b);
                }
                Work::Draw { seq, plan } => {
                    let (used_rows, used_groups) = tx(self.db, engine::used_set).unwrap();
                    let seed = engine::cohort_seed(&c.private_seed, &plan.label);
                    let d = pool::draw(&self.pool, &used_rows, &used_groups, u64::from(plan.counts()[0]), &seed);
                    let t: BTreeMap<String, String> =
                        d.rows.iter().map(|r| (r.row_id.clone(), self.texts[&r.row_id].clone())).collect();
                    tx(self.db, |s| engine::apply_draw(s, &c, seq, &d, &t, &pool::Used::default(), now)).unwrap();
                }
                Work::Seal { seq } => {
                    let b = tx(self.db, |s| engine::prepare_seal(s, &c, seq)).unwrap();
                    for (k, body) in &b.shards {
                        self.eval.insert(k.clone(), body.clone());
                    }
                    self.eval.insert(b.index_key.clone(), b.index.clone());
                    tx(self.db, |s| engine::apply_seal(s, seq, &b, 2, now)).unwrap();
                }
                Work::LoadSeal { seq, source } => {
                    let index = self.eval[&source.index_key].clone();
                    let b = tx(self.db, |s| engine::prepare_load_seal(s, &c, seq, &index)).unwrap();
                    tx(self.db, |s| engine::apply_load_seal(s, &c, seq, &b, now)).unwrap();
                }
                Work::Package { seq } => {
                    let keys: Vec<String> = self
                        .db
                        .conn
                        .prepare("SELECT summary_key FROM jobs WHERE seq = ?1 ORDER BY job")
                        .unwrap()
                        .query_map([seq], |r| r.get(0))
                        .unwrap()
                        .map(Result::unwrap)
                        .collect();
                    let summaries: Vec<engine::JobSummary> =
                        keys.iter().map(|k| serde_json::from_slice(&self.eval[k]).unwrap()).collect();
                    let b = tx(self.db, |s| engine::prepare_package(s, &c, seq, &summaries)).unwrap();
                    tx(self.db, |s| engine::commit_package(s, seq, &b, 6, now)).unwrap();
                    self.packages.push(b);
                }
                Work::Dispatch { kicks, stop, .. } => {
                    for k in kicks {
                        self.send(k);
                    }
                    for name in stop {
                        if let Some(l) = self.lanes.get_mut(&name) {
                            l.armed = false;
                        }
                    }
                    break;
                }
                Work::Settle { seq, microusd, .. } => {
                    // Mirrors `cf_eval`: a ledger that does not answer only
                    // delays the settle, which waits out its retry time.
                    self.settle_calls += 1;
                    if self.ledger_down {
                        tx(self.db, |s| engine::bookkeeping_failed(s, engine::BOOKKEEPING_SETTLE, "ledger_settle", now)).unwrap();
                        break;
                    }
                    tx(self.db, |s| {
                        lanes::mark_settled(s, seq, microusd)?;
                        engine::bookkeeping_done(s, engine::BOOKKEEPING_SETTLE)
                    })
                    .unwrap();
                }
                Work::Purge { seq, prefixes } => {
                    self.eval.retain(|k, _| !prefixes.iter().any(|p| k.starts_with(p)));
                    tx(self.db, |s| lanes::mark_purged(s, seq, now)).unwrap();
                }
                // Mirrors `cf_eval`'s step: the authorization hold, the
                // marker, then one listing page, the single keys and the rows.
                Work::RetireReceipts { seq, prefix, keys, marker } => {
                    use super::retention::contract::AUTHORIZATION_POINTER_KEY;
                    use super::retention::{Hold, apply_hold, hold_of, note_retiring, pointer_ref};
                    let hold = match self.rcpt.get(AUTHORIZATION_POINTER_KEY) {
                        None => Hold::None,
                        Some(p) => {
                            let (kind, r) = pointer_ref(p, &self.cfg.chain).unwrap();
                            hold_of(&kind, &r, &self.rcpt[&r.path]).unwrap()
                        }
                    };
                    if !tx(self.db, |s| apply_hold(s, &hold, seq, now)).unwrap() {
                        continue;
                    }
                    if let Some((key, body)) = marker {
                        self.rcpt.insert(key, body.into_bytes());
                        tx(self.db, |s| note_retiring(s, seq, now)).unwrap();
                    }
                    let page: Vec<String> =
                        self.rcpt.keys().filter(|k| k.starts_with(&prefix)).take(super::retention::PAGE).cloned().collect();
                    if page.is_empty() {
                        for k in &keys {
                            self.rcpt.remove(k);
                        }
                        tx(self.db, |s| super::retention::finish(s, seq, super::retention::PAGE, now)).unwrap();
                    } else {
                        for k in &page {
                            self.rcpt.remove(k);
                        }
                        let n = page.len() as u64;
                        tx(self.db, |s| super::retention::note_page(s, seq, n)).unwrap();
                    }
                }
                // Predecessor notices are driven by the retention tests.
                Work::RetentionNotice { .. } => break,
                // Admission I/O is driven by the admission tests themselves.
                Work::AdmissionVerify { .. } | Work::AdmissionLanes | Work::AdmissionCleanup => break,
                Work::AdmissionSettle { key, .. } => {
                    tx(self.db, |s| crate::eval::admission::charge_settled(s, &key)).unwrap();
                }
                Work::OpenNext { plan, .. } => {
                    tx(self.db, |s| engine::queue(s, &c, &plan, now)).unwrap();
                }
                // Mirrors `cf_eval::run_probe`: a generation probe asks the
                // simulated provider; a scorer probe starts a fresh instance
                // of the probed artifact (the reference, job 0, or the
                // previous reference, job 1) and scores the control document.
                Work::Probe { seq, code, probe } => {
                    let p2 = probe.clone();
                    let ticket = tx(self.db, |s| engine::probe_begin(s, seq, &p2, now)).unwrap();
                    let o = match &probe {
                        engine::Probe::Generation { item, .. } => {
                            let g = outcome(item, (self.generate)(item));
                            let ok = engine::probe_answered(&g);
                            engine::ProbeOutcome { ok, definitive: false, ms: self.probe_ms, nanousd: 1_000_000, note: "down".into() }
                        }
                        engine::Probe::Scorer { body, .. } => {
                            let plan = tx(self.db, |s| engine::cohort(s, seq)).unwrap().plan;
                            let previous = plan.previous_reference.as_ref().is_some_and(|p| body["package_sha256"] == p.package_sha256.as_str());
                            let job = i64::from(previous);
                            self.probe_jobs.push(job);
                            let req = Req { lane: "fault-probe".into(), job, chunk: -1, attempt: 0, tasks: vec![lane_logic::CONTROL_TASK], isolate: true };
                            let (ok, definitive) = match (self.start)(job, false) {
                                Ok(()) => ((self.score)(&req) == Score::Ok, false),
                                Err(RestartFailure::Artifact { definitive, code }) => (false, definitive && code.starts_with("refused")),
                                Err(_) => (false, false),
                            };
                            engine::ProbeOutcome { ok, definitive, ms: self.probe_ms, nanousd: 2_000_000, note: "down".into() }
                        }
                    };
                    self.probes.push(code.clone());
                    self.probe_starts.push((seq, now));
                    self.now += self.probe_ms;
                    let end = self.now;
                    if !tx(self.db, |s| engine::probe_done(s, seq, &code, &ticket, &o, end)).unwrap() {
                        break;
                    }
                }
                Work::AbandonMarker { seq } => {
                    let (key, record) = tx(self.db, |s| engine::abandonment_record(s, &c, seq)).unwrap();
                    if (self.r2)(&key, true) {
                        // The receipts put failed: retried later, and the step goes on.
                        tx(self.db, |s| engine::abandon_marker_failed(s, seq, "abandonment record put", now)).unwrap();
                        continue;
                    }
                    let signed = slop_ninja_protocol::sign(record, &slop_ninja_protocol::fixtures::package_key()).unwrap();
                    self.rcpt.insert(key, signed.to_bytes().unwrap());
                    tx(self.db, |s| engine::abandon_marker_written(s, seq, now)).unwrap();
                }
            }
        }
        progress
    }

    /// Mirrors the Worker's exposed-stock draw (`cf_eval::draw_v3`).
    fn draw_v3(&mut self, seq: i64, plan: &CohortPlan) {
        let c = self.cfg.clone();
        let now = self.now;
        let used = tx(self.db, engine::used_set).unwrap();
        let seed = engine::cohort_seed(&c.private_seed, &plan.label);
        let [n_h, n_m, _] = plan.generated_counts();
        let c_a = u64::from(plan.audit_humans());
        let role = |r: &str| slop_ninja_protocol::domain_hash(&format!("slop_ninja.eval/draw-role/v1/{r}"), &seed).0;
        let mark = |rows: &mut BTreeSet<String>, groups: &mut BTreeSet<String>, d: &pool::Draw| {
            rows.extend(d.rows.iter().map(|r| r.row_id.clone()));
            groups.extend(d.groups.iter().cloned());
        };
        let pool = self.pool.clone();
        let mut drawn: (BTreeSet<String>, BTreeSet<String>) = Default::default();
        let draw_exposed = |want: u64, role_seed: [u8; 32], drawn: &mut (BTreeSet<String>, BTreeSet<String>)| {
            let mut avoid_rows = used.0.clone();
            avoid_rows.extend(drawn.0.iter().cloned());
            let mut avoid_groups = used.1.clone();
            avoid_groups.extend(drawn.1.iter().cloned());
            let mut d = pool::draw(&pool, &avoid_rows, &avoid_groups, want, &role_seed);
            mark(&mut drawn.0, &mut drawn.1, &d);
            let got = d.rows.len() as u64;
            if got < want {
                let again = pool::draw(&pool, &drawn.0, &drawn.1, want - got, &slop_ninja_protocol::domain_hash("again", &role_seed).0);
                mark(&mut drawn.0, &mut drawn.1, &again);
                d.rows.extend(again.rows);
                d.groups.extend(again.groups);
            }
            d
        };
        let negatives = draw_exposed(u64::from(n_h), seed, &mut drawn);
        let mut audits = Vec::new();
        for (r, want) in [("audit-a", c_a), ("audit-b", u64::from(plan.audit_b_humans()))] {
            if want == 0 {
                audits.push(None);
                continue;
            }
            audits.push(Some(draw_exposed(want, role(r), &mut drawn)));
        }
        let p2 = plan.clone();
        let (earlier, ledger_all, ineligible) = if plan.is_probe_build() {
            (Vec::new(), BTreeSet::new(), BTreeSet::new())
        } else {
            let earlier = tx(self.db, |s| v3::reuse_candidates(s, &p2)).unwrap();
            let (all, ineligible) = tx(self.db, |s| v3::ledger_rows(s, &p2)).unwrap();
            (earlier, all, ineligible)
        };
        let earlier: Vec<v3::ReuseCandidate> = earlier.into_iter().filter(|e| !drawn.0.contains(&e.row.row_id)).collect();
        let mut parent_avoid = drawn.0.clone();
        parent_avoid.extend(ledger_all.iter().cloned());
        let fresh = pool::draw(&self.pool, &parent_avoid, &drawn.1, u64::from(n_m), &role("parents"));
        let texts = self.texts.clone();
        let parents = v3::choose_parents(&seed, &negatives.rows, &earlier, &fresh.rows, &texts, n_m, &ineligible);
        let mut probe = Vec::new();
        for p in &plan.reward_v1.as_ref().unwrap().probes {
            let rec: v3::ProbeRecord = serde_json::from_slice(&self.eval[&p.key]).unwrap();
            assert_eq!(lane_logic::sha_hex(&self.eval[&p.key]), p.sha256);
            let mut by_task = BTreeMap::new();
            for (key, sha) in &rec.shards {
                assert_eq!(&lane_logic::sha_hex(&self.eval[key]), sha);
                for line in std::str::from_utf8(&self.eval[key]).unwrap().lines() {
                    let v: Value = serde_json::from_str(line).unwrap();
                    by_task.insert(v["task"].as_u64().unwrap() as u32, v["text"].as_str().unwrap().to_owned());
                }
            }
            probe.push((p.version, rec.docs.iter().map(|d| (d.clone(), by_task[&d.task].clone())).collect()));
        }
        let d = v3::DrawV3 {
            negatives,
            audit_a: audits[0].clone(),
            audit_b: audits[1].clone(),
            fresh: Some(fresh),
            earlier,
            probe,
        };
        tx(self.db, |s| v3::apply_draw(s, &c, seq, &d, &parents, &texts, &pool::Used::default(), now)).unwrap();
    }

    fn artifact_digests(&self, a: &super::plan::ArtifactJson) -> v3::ArtifactDigests {
        let b = &self.eval[&a.manifest_key];
        assert_eq!(lane_logic::sha_hex(b), a.package_sha256);
        v3::artifact_digests(b).unwrap()
    }

    fn seal_v3(&mut self, seq: i64) {
        let c = self.cfg.clone();
        let now = self.now;
        let plan = tx(self.db, |s| Ok::<_, engine::EvalError>(engine::cohort(s, seq)?.plan)).unwrap();
        let inputs = v3::SealInputs {
            reference: self.artifact_digests(&plan.reference),
            predecessor: plan.previous_reference.as_ref().map(|p| self.artifact_digests(p)),
            entrants: plan.entrants.iter().map(|a| self.artifact_digests(a)).collect(),
        };
        let b = tx(self.db, |s| v3::prepare_seal(s, &c, seq, &inputs)).unwrap();
        for (k, body) in &b.shards {
            self.eval.insert(k.clone(), body.clone());
        }
        self.eval.insert(b.index_key.clone(), b.index.clone());
        if let Some((k, bytes)) = &b.probe_record {
            self.eval.insert(k.clone(), bytes.clone());
        }
        tx(self.db, |s| v3::apply_seal(s, seq, &b, 2, now)).unwrap();
    }

    fn package_v3(&mut self, seq: i64) {
        let c = self.cfg.clone();
        let now = self.now;
        let keys: Vec<String> = self
            .db
            .conn
            .prepare("SELECT summary_key FROM jobs WHERE seq = ?1 ORDER BY job")
            .unwrap()
            .query_map([seq], |r| r.get(0))
            .unwrap()
            .map(Result::unwrap)
            .collect();
        let summaries: Vec<engine::JobSummary> = keys.iter().map(|k| serde_json::from_slice(&self.eval[k]).unwrap()).collect();
        let (index, _) = tx(self.db, |s| engine::receipt_index(s, seq)).unwrap();
        let cohort = tx(self.db, |s| engine::cohort(s, seq)).unwrap();
        if cohort.plan.is_probe_build() {
            let v = cohort.plan.reward_v1.as_ref().unwrap().probe_version.unwrap();
            let record = self.eval[&format!("{}/record.json", v3::probe_prefix(&c, v))].clone();
            let f = tx(self.db, |s| v3::prepare_probe_finish(s, &c, &cohort, &summaries, &record)).unwrap();
            for (k, b) in &f.vectors {
                self.eval.insert(k.clone(), b.clone());
            }
            // The Worker's receipts-bucket objects (`cf_eval::package_v3`), by key.
            let p = v3::probe_prefix(&c, v);
            self.rcpt.insert(format!("{p}/receipt-index.signed"), index.to_bytes().unwrap());
            self.rcpt.insert(format!("{p}/chunk-map.json"), b"{}".to_vec());
            tx(self.db, |s| v3::commit_probe_finish(s, seq, &f, now)).unwrap();
            return;
        }
        let st = tx(self.db, |s| v3::state(s, seq)).unwrap();
        let at_seal = v3::index_at_seal(&st).unwrap();
        let (_, vkeys) = tx(self.db, v3::published_index).unwrap();
        let mut fps = BTreeMap::new();
        for f in &at_seal.fingerprints.entries {
            let v = v3::vector_from_bytes(&self.eval[&vkeys[&(f.content, f.version)]]).unwrap();
            assert_eq!(v3::vector_digest(&v), f.fingerprint);
            fps.insert((f.content, f.version), v);
        }
        let (root, count) = (index.digest().unwrap(), index.count());
        let b = tx(self.db, |s| v3::prepare_package(s, &c, &cohort, &summaries, &fps, root, count)).unwrap();
        for (k, v) in &b.vectors {
            self.eval.insert(k.clone(), v.clone());
        }
        let prefix = engine::receipt_prefix(&cohort.label);
        self.rcpt.insert(format!("{prefix}/receipt-index.signed"), index.to_bytes().unwrap());
        self.rcpt.insert(format!("{prefix}/chunk-map.json"), b"{}".to_vec());
        self.rcpt.insert(format!("{prefix}/package.signed"), b.package.to_bytes().unwrap());
        tx(self.db, |s| v3::commit_package(s, seq, &b, 6, now)).unwrap();
        self.packages_v3.push(b);
    }

    fn send(&mut self, k: LaneKick) {
        self.sent += 1;
        if self.drop_every.is_some_and(|n| self.sent.is_multiple_of(n)) {
            return;
        }
        if self.duplicate {
            self.queue.push(k.clone());
        }
        self.queue.push(k);
    }

    /// The queue consumer: each kick reaches its lane's `/kick`, which keeps the
    /// lexicographically larger `(seq, gen)` and arms the alarm.
    fn deliver(&mut self) {
        for k in std::mem::take(&mut self.queue) {
            self.delivered += 1;
            let l = self.lanes.entry(k.name.clone()).or_insert(LaneDo {
                seq: 0,
                kind: k.kind.clone(),
                lane: k.lane,
                generation: 0,
                armed: false,
            });
            if (k.seq, k.generation) > (l.seq, l.generation) {
                *l = LaneDo { seq: k.seq, kind: k.kind.clone(), lane: k.lane, generation: k.generation, armed: true };
            }
        }
    }

    fn tick(&mut self) {
        self.now += self.tick_ms;
        if self.heartbeats {
            heartbeat(self.db, self.now);
        }
        self.step();
        self.deliver();
        let names: Vec<String> = self
            .lanes
            .iter()
            .filter(|(_, l)| l.armed && !self.frozen.iter().any(|k| *k == l.kind))
            .map(|(n, _)| n.clone())
            .collect();
        for name in names {
            self.run_lane(&name);
        }
    }

    /// Ticks until the cohort `seq` leaves its work phases, or `max` ticks.
    fn run(&mut self, seq: i64, max: usize) -> String {
        for _ in 0..max {
            self.tick();
            let p = phase(self.db, seq);
            if !matches!(p.as_str(), "generating" | "sealing" | "scoring" | "packaging") || fault_of(self.db, seq).is_some() {
                // Settle and stop any lane still armed.
                self.tick();
                return p;
            }
        }
        panic!("cohort {seq} did not settle: {}", phase(self.db, seq))
    }

    fn lane_ref(&self, name: &str) -> LaneRef {
        let l = &self.lanes[name];
        LaneRef { seq: l.seq, kind: l.kind.clone(), lane: l.lane, generation: l.generation }
    }

    fn report(&self, r: &LaneRef, state: &str, cold: Option<u64>) -> Assignment {
        let rep = LaneReport {
            state: state.into(),
            cold_ms: cold,
            cold_cpu_ms: cold.map(|_| 45_000),
            started: cold.is_some(),
            active_ms: 1000,
            cpu_ms: 500,
            do_ms: 1000,
            container_up: !matches!(state, "done" | "paused" | "yield"),
            code: None,
            host: cold.map(|_| "Sim CPU".to_owned()),
        };
        tx(self.db, |s| lanes::lane_report(s, r, &rep, self.now)).unwrap()
    }

    fn run_lane(&mut self, name: &str) {
        let r = self.lane_ref(name);
        let a = self.report(&r, "starting", Some(30_000));
        match a.status.as_str() {
            "go" => {}
            "wait" => return,
            "fenced" => {
                self.lanes.get_mut(name).unwrap().armed = false;
                return;
            }
            other => {
                self.report(&r, if other == "paused" { "paused" } else { "done" }, None);
                self.lanes.get_mut(name).unwrap().armed = false;
                return;
            }
        }
        let end = match r.kind.as_str() {
            "score" => self.score_loop(name, &r, &a),
            "gen" => self.gen_loop(name, &r),
            "summary" => self.summary_lane(&r, &a),
            _ => self.bundle_lane(&r, &a),
        };
        match end {
            End::Rearm => {}
            End::Stop(state) => {
                if let Some(state) = state {
                    self.report(&r, state, None);
                }
                self.lanes.get_mut(name).unwrap().armed = false;
            }
        }
    }

    fn score_loop(&mut self, name: &str, r: &LaneRef, a: &Assignment) -> End {
        let mut a = a.clone();
        let mut cache = lane_logic::ShardCache::new();
        self.elapsed = 0;
        for _ in 0..self.claims_per_run {
            // A newer kick replaced this loop's assignment: exit, container kept.
            if self.lanes[name].generation != r.generation {
                return End::Stop(None);
            }
            if self.elapsed > a.step_budget_ms {
                return End::Rearm;
            }
            // The cold path holds no claim; an artifact that failed needs none.
            if !a.artifact_failed && !self.loaded.contains(name) {
                let job = a.job.unwrap();
                self.elapsed += self.restart_ms;
                match (self.start)(job, false) {
                    Ok(()) => {
                        self.loaded.insert(name.to_owned());
                    }
                    Err(RestartFailure::Artifact { code, definitive }) => {
                        let f = lanes::ArtifactFailure { lane: r.clone(), job, code: code.into(), definitive };
                        if tx(self.db, |s| lanes::artifact_failure(s, &f.lane, f.job, &f.code, f.definitive, self.now)).unwrap() {
                            a.artifact_failed = true;
                            continue;
                        }
                        return End::Rearm;
                    }
                    Err(RestartFailure::Infra(code)) => {
                        let rep = LaneReport { state: "start_failed".into(), code: Some(code.into()), ..Default::default() };
                        tx(self.db, |s| lanes::lane_report(s, r, &rep, self.now)).unwrap();
                        return End::Rearm;
                    }
                }
            }
            let reply = tx(self.db, |s| lanes::claim_chunk(s, r, name, self.now)).unwrap();
            match reply {
                ClaimReply::Fenced => return End::Stop(None),
                ClaimReply::Paused => return End::Stop(Some("paused")),
                ClaimReply::Done => return End::Stop(Some("done")),
                ClaimReply::Wait { .. } => return End::Rearm,
                ClaimReply::Chunk(c) => {
                    if c.model_error && !a.artifact_failed {
                        a = self.report(r, "running", None);
                    }
                    let seed = engine::cohort_seed(&self.cfg.private_seed, &a.label);
                    let prefix = engine::receipt_prefix(&a.label);
                    let art = a.artifact.clone().unwrap();
                    let cx = ChunkCtx { r, name, a: &a, art: &art, seed: &seed, prefix: &prefix };
                    let mut io = SimIo { sim: self, lane: name.to_owned(), r: r.clone(), claim: c.clone(), a: a.clone(), die: None };
                    match lane_logic::block_on(lane_logic::run_chunk(&mut io, &cx, &c, &mut cache)) {
                        ChunkEnd::Next => {}
                        ChunkEnd::Rearm(_) | ChunkEnd::Throw => return End::Rearm,
                        ChunkEnd::Died => return End::Stop(None),
                    }
                }
            }
        }
        End::Rearm
    }

    fn gen_loop(&mut self, name: &str, r: &LaneRef) -> End {
        let c2 = self.cfg.clone();
        let mut wants: Option<BTreeMap<String, u32>> = None;
        let grant = self.grant;
        for _ in 0..self.claims_per_run.min(50) {
            let gens: Vec<String> = self.db.conn.query_row("SELECT plan FROM cohorts WHERE seq = ?1", [r.seq], |row| {
                let p: String = row.get(0)?;
                Ok(serde_json::from_str::<CohortPlan>(&p).unwrap().generators)
            }).unwrap();
            let grants: BTreeMap<String, u32> = match &wants {
                Some(w) => w.iter().map(|(m, n)| (m.clone(), (*n).min(grant))).collect(),
                None => gens.iter().map(|g| (g.clone(), grant)).collect(),
            };
            let reply = tx(self.db, |s| lanes::gen_claim(s, &c2, r, name, &grants, self.now)).unwrap();
            match reply {
                GenClaimReply::Items { items, wants: w } => {
                    let out: Vec<GenOutcome> = items.iter().map(|it| outcome(it, (self.generate)(it))).collect();
                    tx(self.db, |s| lanes::gen_commit(s, &c2, r, &out, self.now)).unwrap();
                    wants = Some(w);
                }
                GenClaimReply::Wait { ms: 0, wants: w } => wants = Some(w),
                GenClaimReply::Wait { .. } => return End::Rearm,
                GenClaimReply::Done => return End::Stop(Some("done")),
                GenClaimReply::Paused => return End::Stop(Some("paused")),
                GenClaimReply::Fenced => return End::Stop(None),
            }
        }
        End::Rearm
    }

    fn summary_lane(&mut self, r: &LaneRef, a: &Assignment) -> End {
        let job = a.job.unwrap();
        let mut rows = Vec::new();
        let mut after = -1;
        loop {
            let page = tx(self.db, |s| engine::job_preds(s, r.seq, job, after, 7)).unwrap();
            let last = page.last().map(|p| i64::from(p.task));
            let n = page.len();
            rows.extend(page);
            match last {
                Some(t) if n == 7 => after = t,
                _ => break,
            }
        }
        let sm = engine::summarize_job(job, &a.artifact.as_ref().unwrap().id, &rows).unwrap();
        let bytes = serde_json::to_vec(&sm).unwrap();
        let key = lane_logic::summary_key(&a.label, job);
        self.eval.insert(key.clone(), bytes.clone());
        let rep = lanes::SummaryReport { job, key, sha256: lane_logic::sha_hex(&bytes), count: sm.count, rows_sha256: sm.rows_sha256 };
        assert!(tx(self.db, |s| lanes::summary_done(s, r, &rep, self.now)).unwrap());
        End::Stop(None)
    }

    fn bundle_lane(&mut self, r: &LaneRef, a: &Assignment) -> End {
        let job = a.job.unwrap();
        let objects = tx(self.db, |s| lanes::job_objects(s, r.seq, job)).unwrap();
        let mut bundle = Vec::new();
        for (key, sha, _, _) in &objects {
            let b = &self.rcpt[key];
            assert_eq!(&lane_logic::sha_hex(b), sha);
            bundle.extend_from_slice(b);
        }
        let mut expected = BTreeMap::new();
        let mut after = 0;
        loop {
            let page = tx(self.db, |s| lanes::job_receipts(s, r.seq, job, after, 11)).unwrap();
            for (_, id, sha, len) in &page {
                let d: [u8; 32] = hex::decode(sha).unwrap().try_into().unwrap();
                expected.insert(engine::parse_digest(id).unwrap(), (d, *len as u32));
            }
            match page.last() {
                Some(p) if page.len() == 11 => after = p.0,
                _ => break,
            }
        }
        let n = receipts::verify_bundle(&bundle, &expected).unwrap();
        let key = lane_logic::bundle_key(&a.label, job);
        self.rcpt.insert(key.clone(), bundle.clone());
        let rep = lanes::BundleReport { job, key, sha256: lane_logic::sha_hex(&bundle), bytes: bundle.len() as u64, receipts: n };
        assert!(tx(self.db, |s| lanes::bundle_done(s, r, &rep, self.now)).unwrap());
        End::Stop(None)
    }
}

enum End {
    Rearm,
    Stop(Option<&'static str>),
}

/// A simulated scoring lane's world for `lane_logic::run_chunk`: the two
/// buckets, the cohort object (through the Worker's own routes), the scorer
/// hook and a virtual invocation clock.
struct SimIo<'s, 'a> {
    sim: &'s mut Sim<'a>,
    lane: String,
    r: LaneRef,
    claim: ChunkClaim,
    a: Assignment,
    /// A lane death the scorer hook asked for, after the request.
    die: Option<Score>,
}

impl LaneIo for SimIo<'_, '_> {
    async fn eval_get(&mut self, key: &str) -> Result<Option<Vec<u8>>, ()> {
        if (self.sim.r2)(key, false) { Err(()) } else { Ok(self.sim.eval.get(key).cloned()) }
    }
    async fn eval_put(&mut self, key: &str, body: Vec<u8>) -> Result<(), ()> {
        if (self.sim.r2)(key, true) {
            return Err(());
        }
        self.sim.eval.insert(key.to_owned(), body);
        Ok(())
    }
    async fn rcpt_get(&mut self, key: &str) -> Result<Option<Vec<u8>>, ()> {
        if (self.sim.r2)(key, false) { Err(()) } else { Ok(self.sim.rcpt.get(key).cloned()) }
    }
    async fn rcpt_put(&mut self, key: &str, body: Vec<u8>) -> Result<(), ()> {
        if (self.sim.r2)(key, true) {
            return Err(());
        }
        self.sim.rcpt.insert(key.to_owned(), body);
        Ok(())
    }
    async fn cohort(&mut self, path: &str, body: Value) -> Result<Value, CohortError> {
        let bytes = serde_json::to_vec(&body).unwrap();
        let (c, now) = (self.sim.cfg.clone(), self.sim.now);
        let r = tx(self.sim.db, |s| lanes::scoring_route(s, &c, path, &bytes, now));
        let v = match r {
            Ok(Some(v)) => v,
            Ok(None) => panic!("no scoring route {path}"),
            Err(_) => return Err(CohortError::Refused),
        };
        if path == "/lane/commit" && v.get("Committed").is_some() {
            // A lane that lost the reply retries: the exact repeat is `already`.
            let again = tx(self.sim.db, |s| lanes::scoring_route(s, &c, path, &bytes, now)).unwrap().unwrap();
            assert_eq!(again, json!("Already"));
        }
        Ok(v)
    }
    async fn score(&mut self, docs: &[(u32, String, String)], _: u64) -> Result<(Vec<Value>, engine::CallTiming), ScoreFailure> {
        self.sim.calls += 1;
        self.sim.elapsed += self.sim.doc_ms;
        let req = Req {
            lane: self.lane.clone(),
            job: self.claim.job,
            chunk: self.claim.chunk,
            attempt: self.claim.attempt,
            tasks: docs.iter().map(|d| d.0).collect(),
            isolate: self.claim.isolate,
        };
        let results = match &self.sim.predict {
            Some(p) => predicted(self.sim.db, self.r.seq, self.claim.job, docs, p.as_ref()),
            None => fake_results(self.sim.db, self.r.seq, docs),
        };
        let ok = || (results.clone(), engine::CallTiming { call_ms: 9_000, server_ms: 8_500, load_ms: 0, cpu_ms: 30_000 });
        match (self.sim.score)(&req) {
            Score::Ok => Ok(ok()),
            Score::Crash => {
                self.sim.loaded.remove(&self.lane);
                Err(ScoreFailure::Died)
            }
            Score::Fail => Err(ScoreFailure::Failed),
            Score::NotDispatched => Err(ScoreFailure::NotDispatched),
            die => {
                self.die = Some(die);
                Ok(ok())
            }
        }
    }
    async fn restart(&mut self) -> Result<(), RestartFailure> {
        self.sim.restarts += 1;
        self.sim.elapsed += self.sim.restart_ms;
        self.sim.loaded.remove(&self.lane);
        (self.sim.start)(self.claim.job, true)?;
        self.sim.loaded.insert(self.lane.clone());
        Ok(())
    }
    fn restart_bound_ms(&self) -> u64 {
        self.sim.restart_ms
    }
    async fn may_run(&mut self, ms: u64) -> bool {
        self.sim.elapsed + ms <= self.a.step_budget_ms && self.sim.lanes[&self.lane].generation == self.r.generation
    }
    fn now_ms(&self) -> u64 {
        self.sim.now
    }
    async fn sleep(&mut self, _: u64) {}
    async fn fault(&mut self, f: Fault) -> bool {
        matches!((f, &self.die), (Fault::DieBeforePut, Some(Score::DieBeforePut)) | (Fault::DieAfterPut, Some(Score::DieAfterPut)))
    }
    async fn meter(&mut self) -> lanes::Meter {
        lanes::Meter { active_ms: 1_000, cpu_ms: 500, do_ms: 1_000 }
    }
}

fn phase(db: &Sqlite, seq: i64) -> String {
    db.conn.query_row("SELECT phase FROM cohorts WHERE seq = ?1", [seq], |r| r.get(0)).unwrap()
}

fn fault_of(db: &Sqlite, seq: i64) -> Option<String> {
    db.conn.query_row("SELECT fault FROM cohorts WHERE seq = ?1", [seq], |r| r.get(0)).unwrap()
}

fn status(db: &Sqlite, now: u64) -> Value {
    tx(db, |s| engine::status(s, now)).unwrap()
}

fn open_cohort(db: &Sqlite, p: &CohortPlan, first_epoch: u64, now: u64) -> i64 {
    let bytes = serde_json::to_vec(p).unwrap();
    let seq = tx(db, |s| engine::queue(s, &cfg(), &bytes, now)).unwrap();
    kick_at(db, first_epoch, now);
    kick_at(db, first_epoch + 1, now);
    kick_at(db, first_epoch + 2, now);
    seq
}

/// Receipt attempt numbers and outcome tags for one task of one job.
fn numbers(db: &Sqlite, seq: i64, job: i64, task: i64) -> Vec<(i64, i64)> {
    db.conn
        .prepare("SELECT number, outcome FROM receipts WHERE seq = ?1 AND job = ?2 AND task = ?3 ORDER BY number")
        .unwrap()
        .query_map(rusqlite::params![seq, job, task], |r| Ok((r.get(0)?, r.get(1)?)))
        .unwrap()
        .map(Result::unwrap)
        .collect()
}

/// Verifies a package's bundles against its index, as `eval_verify` does.
fn verify(sim: &Sim<'_>, b: &engine::PackageBuild) -> Vec<slop_ninja_protocol::AttemptReceipt> {
    let objects: Vec<Vec<u8>> = b.chunk_map["objects"]
        .as_array()
        .unwrap()
        .iter()
        .map(|o| {
            let bytes = sim.rcpt[o["key"].as_str().unwrap()].clone();
            assert_eq!(lane_logic::sha_hex(&bytes), o["sha256"].as_str().unwrap());
            bytes
        })
        .collect();
    let v = receipts::verify_chunks(&b.index, &objects).unwrap();
    b.package.check_receipts(&b.index).unwrap();
    b.package.check_manifest(&b.manifest).unwrap();
    b.target.check_package(&b.package).unwrap();
    v
}

fn wide(n: u32, entrants: u16) -> CohortPlan {
    let mut p = plan(n);
    p.scorer.chunk_docs = 4;
    p.fanout.as_mut().unwrap().score.lane_docs = 8;
    p.entrants = (1..=entrants).map(|u| super::plan::tests::artifact(super::plan::tests::identity(u, u as u8), 0x22)).collect();
    p.allow_list = (1..=entrants).map(|u| super::plan::tests::identity(u, u as u8)).collect();
    p
}

// --------------------------------------------------------------------- tests

#[test]
fn a_cohort_runs_end_to_end_through_lanes_with_verifiable_bundles() {
    let db = Sqlite::open();
    sql::migrate(&db, &EVAL_COHORT_SCHEMA).unwrap();
    let mut now = 1_000_000;
    let p = wide(20, 2);
    let bytes = serde_json::to_vec(&p).unwrap();
    let seq = tx(&db, |s| engine::queue(s, &cfg(), &bytes, now)).unwrap();
    // A label queues once: the same bytes again (a retried POST) are the same
    // cohort; other bytes under it are refused.
    assert_eq!(tx(&db, |s| engine::queue(s, &cfg(), &bytes, now)).unwrap(), seq);
    let mut other = p.clone();
    other.n += 1;
    assert!(tx(&db, |s| engine::queue(s, &cfg(), &serde_json::to_vec(&other).unwrap(), now)).is_err());
    assert!(kick_at(&db, 10, now).is_empty());
    assert_eq!(kick_at(&db, 11, now), ["testnet-rehearsal-1:admission"]);
    assert_eq!(kick_at(&db, 12, now), ["testnet-rehearsal-1:generating"]);
    now += 10;
    let mut sim = Sim::new(&db, cfg(), now);
    let mut first = true;
    sim.generate = Box::new(move |it| {
        if std::mem::take(&mut first) { Ok("Here is the rewritten passage: nothing".into()) } else { Ok(answer(it)) }
    });
    assert_eq!(sim.run(seq, 400), "transition");
    let st = status(&db, sim.now);
    let c0 = &st["cohorts"][0];
    assert_eq!(c0["task_count"], 20);
    assert_eq!(c0["rejects"]["preamble"], 1, "one rejected output was retried on the next generator");
    let lanes = &c0["lanes"]["kinds"];
    // 3 jobs x ceil(20 / 8) = 3 lanes each; 4 generation lanes; a summary and bundle lane per job.
    assert_eq!(lanes["score"]["lanes"], 9);
    assert_eq!(lanes["gen"]["lanes"], 3, "min(4, ceil(12 / 4))");
    assert_eq!((lanes["summary"]["lanes"].as_i64(), lanes["bundle"]["lanes"].as_i64()), (Some(3), Some(3)));
    assert!(c0["lanes"]["cold_ms"]["n"].as_i64().unwrap() >= 9);
    assert!(!st.to_string().contains("harbour") && !st.to_string().contains("prose prose"), "status is content-free");
    let b = sim.packages.pop().expect("packaged");
    let verified = verify(&sim, &b);
    assert_eq!(verified.len(), 60, "3 jobs x 20 tasks, one attempt each");
    assert_eq!(b.chunk_map["objects"].as_array().unwrap().len(), 3, "one bundle per job");
    assert_eq!(b.components["clusters"], json!([[0, 1, 2]]));
    let id = |uid: u16, byte: u8| NeuronIdentity { uid, hotkey: AccountId([byte; 32]), registration_block: 1000 + u64::from(uid) };
    let snap = RegistrationSnapshot {
        chain: cfg().chain,
        block: 5000,
        block_hash: slop_ninja_protocol::Digest([1; 32]),
        neurons: vec![id(0, 0xb0), id(1, 1), id(2, 2)],
    };
    let row = reward::derive_row(&b.target, &b.package, &snap).unwrap();
    assert_eq!((row.entries[0].uid, row.entries[0].weight), (0, u16::MAX));
    // The cohort closes at opened + K and its spend settles once.
    assert!(kick_at(&db, 13, sim.now).is_empty());
    assert!(kick_at(&db, 14, sim.now).is_empty());
    assert_eq!(kick_at(&db, 15, sim.now), ["testnet-rehearsal-1:closed"]);
    sim.tick();
    sim.tick();
    let settled: Option<i64> = db.conn.query_row("SELECT settled_microusd FROM cohorts WHERE seq = 1", [], |r| r.get(0)).unwrap();
    assert!(settled.unwrap() > 0);
    // The private results objects and summaries (per-document predictions) are gone.
    assert!(sim.eval.keys().all(|k| !k.contains("/results/") && !k.contains("/summaries/")), "purged at close");
    assert!(sim.eval.keys().any(|k| k.contains("/batch/")), "the sealed batch stays");
    // A second cohort never reuses a human text or source group.
    let mut p2 = wide(20, 1);
    p2.cohort = 2;
    p2.label = "testnet-rehearsal-2".into();
    let seq2 = tx(&db, |s| engine::queue(s, &cfg(), &serde_json::to_vec(&p2).unwrap(), sim.now)).unwrap();
    assert_eq!(kick_at(&db, 16, sim.now), ["testnet-rehearsal-2:admission"]);
    assert_eq!(kick_at(&db, 17, sim.now), ["testnet-rehearsal-2:generating"]);
    sim.generate = Box::new(|it| Ok(answer(it)));
    assert_eq!(sim.run(seq2, 400), "transition");
    let used = |seq: i64| -> BTreeSet<String> {
        db.conn
            .prepare("SELECT row_id FROM pool_used WHERE seq = ?1")
            .unwrap()
            .query_map([seq], |r| r.get(0))
            .unwrap()
            .map(Result::unwrap)
            .collect()
    };
    assert!(!used(2).is_empty() && used(1).is_disjoint(&used(2)));
    assert_eq!(sim.packages.pop().unwrap().target.predecessor, Some(b.target.digest().unwrap()));
}

#[test]
fn lease_expiry_reconciles_durable_objects_before_any_new_attempt() {
    for after_put in [false, true] {
        let db = Sqlite::open();
        sql::migrate(&db, &EVAL_COHORT_SCHEMA).unwrap();
        let seq = open_cohort(&db, &wide(20, 1), 10, 1_000_000);
        let mut sim = Sim::new(&db, cfg(), 1_000_010);
        sim.tick_ms = 5_000;
        let mut died = false;
        sim.score = Box::new(move |q| {
            if q.job == 0 && q.chunk == 0 && !died {
                died = true;
                return if after_put { Score::DieAfterPut } else { Score::DieBeforePut };
            }
            Score::Ok
        });
        assert_eq!(sim.run(seq, 600), "transition");
        let b = sim.packages.pop().unwrap();
        verify(&sim, &b);
        let st = status(&db, sim.now);
        let score = &st["cohorts"][0]["lanes"]["kinds"]["score"];
        if after_put {
            // The stored attempt was committed with no compute: one receipt, attempt 1.
            for task in 0..4 {
                assert_eq!(numbers(&db, seq, 0, task), [(1, 1)]);
            }
            assert_eq!(score["reconciled"], 1);
            assert_eq!(score["lost_attempts"], 0);
        } else {
            // Nothing was stored: attempt 1 is recorded lost, attempt 2 resolves it.
            for task in 0..4 {
                assert_eq!(numbers(&db, seq, 0, task), [(1, 5), (2, 1)]);
            }
            assert_eq!(score["lost_attempts"], 1);
        }
    }
}

#[test]
fn a_crashed_lane_is_rekicked_and_its_old_generation_is_fenced() {
    let db = Sqlite::open();
    sql::migrate(&db, &EVAL_COHORT_SCHEMA).unwrap();
    let seq = open_cohort(&db, &wide(20, 0), 10, 1_000_000);
    let mut sim = Sim::new(&db, cfg(), 1_000_010);
    sim.tick_ms = 10_000;
    sim.claims_per_run = 1;
    // Lane 0's invocation dies on its first claim and never reports again.
    let died = std::rc::Rc::new(std::cell::Cell::new(false));
    let d2 = died.clone();
    sim.score = Box::new(move |q| {
        if q.lane.ends_with("lane-0000") && !d2.get() {
            d2.set(true);
            Score::DieBeforePut
        } else {
            Score::Ok
        }
    });
    for _ in 0..200 {
        sim.tick();
        if died.get() {
            break;
        }
    }
    assert!(died.get());
    let name = lanes::lane_name("main", "score", 0);
    let r_old = sim.lane_ref(&name);
    sim.lanes.get_mut(&name).unwrap().armed = false;
    // The sweep re-kicks it once it is stale (3 x the 60 s lease) and work is left.
    for _ in 0..30 {
        sim.tick();
    }
    let r_new = sim.lane_ref(&name);
    assert!(r_new.generation > r_old.generation, "re-kicked with gen + 1");
    // The old loop, were it alive, is fenced from claims.
    assert_eq!(tx(&db, |s| lanes::claim_chunk(s, &r_old, &name, sim.now)).unwrap(), ClaimReply::Fenced);
    assert_eq!(sim.run(seq, 400), "transition");
    let st = status(&db, sim.now);
    assert!(st["cohorts"][0]["lanes"]["kinds"]["score"]["rekicks"].as_i64().unwrap() >= 1);
    for task in 0..4 {
        assert_eq!(numbers(&db, seq, 0, task), [(1, 5), (2, 1)], "the lost attempt has its receipt");
    }
    verify(&sim, &sim.packages[0]);
}

#[test]
fn a_container_crash_isolates_and_a_reproduced_crash_counts_against_that_job_only() {
    let db = Sqlite::open();
    sql::migrate(&db, &EVAL_COHORT_SCHEMA).unwrap();
    let seq = open_cohort(&db, &wide(20, 2), 10, 1_000_000);
    let mut sim = Sim::new(&db, cfg(), 1_000_010);
    // Job 1's task 5 crashes (OOMs) its container every time, alone or not.
    // Job 2's chunk 0 crashes once (a transient fault): isolation resolves it.
    let mut transient = true;
    sim.score = Box::new(move |q| {
        if q.job == 1 && q.tasks.contains(&5) {
            return Score::Crash;
        }
        if q.job == 2 && q.chunk == 0 && !q.isolate && std::mem::take(&mut transient) {
            return Score::Crash;
        }
        Score::Ok
    });
    assert_eq!(sim.run(seq, 400), "transition", "the cohort proceeds");
    let b = sim.packages.pop().unwrap();
    verify(&sim, &b);
    // Task 5 of job 1: an infrastructure error (the chunk crash), then a model error.
    assert_eq!(numbers(&db, seq, 1, 5), [(1, 5), (2, 2)]);
    // Its chunk-mates crashed with it, then resolved in isolation.
    assert_eq!(numbers(&db, seq, 1, 4), [(1, 5), (2, 1)]);
    // The same task of the other jobs is an ordinary prediction.
    assert_eq!(numbers(&db, seq, 0, 5), [(1, 1)]);
    assert_eq!(numbers(&db, seq, 2, 0), [(1, 5), (2, 1)], "a crash that does not reproduce is never attributed");
    let jobs = &b.components["jobs"];
    assert_eq!(jobs[1]["isolated_crashes"], 1);
    assert_eq!(jobs[2]["isolated_crashes"], 0);
    assert_eq!(b.package.scores.iter().map(|s| s.failed).collect::<Vec<_>>(), [1, 0], "utility 0 for that document only");
    assert!(b.package.scores[0].score < b.package.baseline);
    assert_eq!(b.package.scores[1].score, b.package.baseline);
}

#[test]
fn duplicate_and_missing_wakeups_change_nothing_but_timing() {
    let db = Sqlite::open();
    sql::migrate(&db, &EVAL_COHORT_SCHEMA).unwrap();
    let seq = open_cohort(&db, &wide(20, 1), 10, 1_000_000);
    let mut sim = Sim::new(&db, cfg(), 1_000_010);
    sim.tick_ms = 5_000;
    sim.duplicate = true;
    sim.drop_every = Some(4);
    assert_eq!(sim.run(seq, 800), "transition");
    assert!(sim.delivered > sim.sent, "kicks were delivered twice");
    let b = sim.packages.pop().unwrap();
    assert_eq!(verify(&sim, &b).len(), 40, "no duplicate attempt: one receipt per task per job");
    for job in 0..2 {
        for task in 0..20 {
            assert_eq!(numbers(&db, seq, job, task), [(1, 1)]);
        }
    }
    let st = status(&db, sim.now);
    let kinds = &st["cohorts"][0]["lanes"]["kinds"];
    let rekicks: i64 = ["gen", "score", "summary", "bundle"].iter().map(|k| kinds[k]["rekicks"].as_i64().unwrap_or(0)).sum();
    assert!(rekicks > 0, "the sweep re-sent the lost kicks");
}

#[test]
fn a_slow_cold_path_that_reports_is_never_rekicked() {
    // The review's livelock: a lane reporting `starting` at alarm entry and every
    // 20 s of a 60 s cold path must not be re-kicked by the 10 s sweep.
    let db = Sqlite::open();
    sql::migrate(&db, &EVAL_COHORT_SCHEMA).unwrap();
    let seq = open_cohort(&db, &wide(20, 0), 10, 1_000_000);
    let mut sim = Sim::new(&db, cfg(), 1_000_010);
    sim.frozen = vec!["score"];
    for _ in 0..100 {
        sim.tick();
        if phase(&db, seq) == "scoring" {
            break;
        }
    }
    let kicked: Vec<(i64, i64, String)> = db
        .conn
        .prepare("SELECT lane, gen, name FROM lanes WHERE seq = ?1 AND kind = 'score' AND status = 'kicked' ORDER BY lane")
        .unwrap()
        .query_map([seq], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    assert_eq!(kicked.len(), 3);
    let c = tx(&db, engine::active).unwrap().unwrap();
    let cf = cfg();
    let mut now = sim.now;
    let (lane, generation, name) = kicked[0].clone();
    let r = LaneRef { seq, kind: "score".into(), lane, generation };
    let rep = |state: &str| LaneReport { state: state.into(), container_up: true, ..Default::default() };
    // Alarm entry, 2 s after the kick, before the cold path.
    now += 2_000;
    assert_eq!(tx(&db, |s| lanes::lane_report(s, &r, &rep("starting"), now)).unwrap().status, "go");
    let mut silent_rekicked = false;
    for step in 1..=7 {
        // 10 s sweeps across the 60 s cold path, with a report every 20 s.
        now += 10_000;
        heartbeat(&db, now);
        if step % 2 == 0 {
            tx(&db, |s| lanes::lane_report(s, &r, &rep("starting"), now)).unwrap();
        }
        let again = tx(&db, |s| lanes::lanes_due(s, &cf, &c, &["score"], now)).unwrap();
        assert!(again.iter().all(|x| x.lane != lane), "re-kicked a lane that reports (step {step})");
        silent_rekicked |= again.iter().any(|x| x.lane == kicked[1].0);
    }
    assert!(silent_rekicked, "a kicked lane that never reports is kicked again after 30 s");
    // Once loaded, the reporting lane claims normally at its own generation.
    assert!(matches!(tx(&db, |s| lanes::claim_chunk(s, &r, &name, now)).unwrap(), ClaimReply::Chunk(_)));
}

#[test]
fn coordinator_silence_pauses_lanes_and_the_same_cohort_resumes() {
    let db = Sqlite::open();
    sql::migrate(&db, &EVAL_COHORT_SCHEMA).unwrap();
    let seq = open_cohort(&db, &wide(40, 1), 10, 1_000_000);
    let mut sim = Sim::new(&db, cfg(), 1_000_010);
    sim.tick_ms = 2_000;
    sim.claims_per_run = 1;
    for _ in 0..400 {
        sim.tick();
        let preds: i64 = db.conn.query_row("SELECT COUNT(*) FROM preds", [], |r| r.get(0)).unwrap();
        if phase(&db, seq) == "scoring" && preds > 0 {
            break;
        }
    }
    assert_eq!(phase(&db, seq), "scoring");
    // Silence: no heartbeat for more than 10 minutes, then several more ticks.
    sim.heartbeats = false;
    sim.tick_ms = 11 * 60_000;
    sim.tick();
    sim.tick_ms = 60_000;
    for _ in 0..5 {
        sim.tick();
    }
    let st = status(&db, sim.now);
    let c0 = &st["cohorts"][0];
    assert_eq!(c0["phase"], "scoring", "nothing expires or is skipped");
    let by = &c0["lanes"]["kinds"]["score"]["by_status"];
    assert!(by["paused"].as_i64().unwrap_or(0) > 0, "lanes paused at their next claim: {by}");
    assert!(sim.lanes.values().all(|l| !l.armed), "paused lanes do not re-arm");
    let r = sim.lane_ref(&lanes::lane_name("main", "score", 0));
    assert_eq!(tx(&db, |s| lanes::claim_chunk(s, &r, "x", sim.now)).unwrap(), ClaimReply::Paused);
    // Resume: the next heartbeat's sweep re-kicks the paused lanes.
    sim.heartbeats = true;
    sim.tick_ms = 2_000;
    assert_eq!(sim.run(seq, 400), "transition");
    let b = sim.packages.pop().unwrap();
    assert_eq!(verify(&sim, &b).len(), 80);
}

#[test]
fn a_hold_is_silence_for_claims() {
    let db = Sqlite::open();
    sql::migrate(&db, &EVAL_COHORT_SCHEMA).unwrap();
    let seq = open_cohort(&db, &wide(20, 0), 10, 1_000_000);
    let mut sim = Sim::new(&db, cfg(), 1_000_010);
    sim.claims_per_run = 1;
    for _ in 0..100 {
        sim.tick();
        if phase(&db, seq) == "scoring" {
            break;
        }
    }
    tx(&db, |s| lanes::set_hold(s, true)).unwrap();
    for _ in 0..20 {
        sim.tick();
    }
    assert_eq!(phase(&db, seq), "scoring", "held: no claim is granted");
    assert!(sim.lanes.values().filter(|l| l.kind == "score").all(|l| !l.armed));
    tx(&db, |s| lanes::set_hold(s, false)).unwrap();
    assert_eq!(sim.run(seq, 400), "transition");
}

#[test]
fn throttles_use_no_attempt_and_keep_the_generator_and_reservations_bound_spend() {
    let db = Sqlite::open();
    sql::migrate(&db, &EVAL_COHORT_SCHEMA).unwrap();
    let mut p = wide(20, 0);
    p.generators = vec!["@cf/meta/llama-3.2-3b-instruct".into(), "@cf/ibm-granite/granite-4.0-h-micro".into()];
    let seq = open_cohort(&db, &p, 10, 1_000_000);
    let mut sim = Sim::new(&db, cfg(), 1_000_010);
    sim.tick_ms = 3_000;
    // Every item's first two calls are throttled.
    let gens: std::rc::Rc<std::cell::RefCell<BTreeMap<i64, Vec<String>>>> = Default::default();
    let g2 = gens.clone();
    sim.generate = Box::new(move |it| {
        let mut g = g2.borrow_mut();
        let calls = g.entry(it.idx).or_default();
        calls.push(it.generator.clone());
        if calls.len() <= 2 { Err(Reject::Throttled) } else { Ok(answer(it)) }
    });
    assert_eq!(sim.run(seq, 800), "transition");
    drop(sim);
    let gens = gens.borrow();
    let (attempts, throttles): (i64, i64) = db
        .conn
        .query_row("SELECT SUM(attempts), SUM(throttles) FROM docs WHERE kind != 'human_only'", [], |r| Ok((r.get(0)?, r.get(1)?)))
        .unwrap();
    assert_eq!(attempts, 12, "one answer per item: throttles used none");
    assert_eq!(throttles, 24);
    assert!(gens.values().all(|g| g.iter().all(|x| x == &g[0])), "the generator never rotated on a throttle");
    let reserved: i64 = db.conn.query_row("SELECT gen_reserved_microusd FROM cohorts WHERE seq = 1", [], |r| r.get(0)).unwrap();
    assert_eq!(reserved, 0, "every reservation was released");
}

#[test]
fn concurrent_generation_claims_past_the_budget_alert_and_late_results_are_fenced() {
    let db = Sqlite::open();
    sql::migrate(&db, &EVAL_COHORT_SCHEMA).unwrap();
    let (pool, texts) = pool_fixture();
    let mut p = wide(20, 0);
    // Less than one item's upper bound: every claim passes the guard.
    p.generation_budget_microusd = 100;
    let seq = open_cohort(&db, &p, 10, 1_000_000);
    let c = cfg();
    let now = 1_000_100;
    let Some(Work::Draw { plan, .. }) = tx(&db, |s| engine::next_work(s, &c, now)).unwrap() else { panic!("draw") };
    let seed = engine::cohort_seed(&c.private_seed, &plan.label);
    let d = pool::draw(&pool, &BTreeSet::new(), &BTreeSet::new(), 8, &seed);
    let t: BTreeMap<String, String> = d.rows.iter().map(|r| (r.row_id.clone(), texts[&r.row_id].clone())).collect();
    assert!(tx(&db, |s| engine::apply_draw(s, &c, seq, &d, &t, &pool::Used::default(), now)).unwrap());
    tx(&db, |s| engine::next_work(s, &c, now)).unwrap();
    let grants: BTreeMap<String, u32> = [("@cf/meta/llama-3.2-3b-instruct".to_owned(), 3)].into();
    let lane = |l: i64| {
        let g: i64 = db.conn.query_row("SELECT gen FROM lanes WHERE seq = 1 AND kind = 'gen' AND lane = ?1", [l], |r| r.get(0)).unwrap();
        LaneRef { seq, kind: "gen".into(), lane: l, generation: g }
    };
    let (a, b) = (lane(0), lane(1));
    let GenClaimReply::Items { items: first, .. } = tx(&db, |s| lanes::gen_claim(s, &c, &a, "a", &grants, now)).unwrap() else { panic!() };
    let second = tx(&db, |s| lanes::gen_claim(s, &c, &b, "b", &grants, now)).unwrap();
    let reserved: i64 = db.conn.query_row("SELECT gen_reserved_microusd FROM cohorts WHERE seq = 1", [], |r| r.get(0)).unwrap();
    assert!(!first.is_empty());
    // Spend is tracked, never capped: past the budget the second lane still
    // claims, and the cohort's budget alert is raised.
    assert!(reserved as u64 > p.generation_budget_microusd);
    let GenClaimReply::Items { items: second, .. } = second else { panic!("the budget refuses nothing: {second:?}") };
    assert!(second.iter().all(|it| first.iter().all(|f| f.idx != it.idx)), "no item is claimed twice");
    assert_eq!(status(&db, now)["cohorts"][0]["budget_alert"], "generation_budget");
    let held: i64 = db.conn.query_row("SELECT gen_reserved_microusd FROM cohorts WHERE seq = 1", [], |r| r.get(0)).unwrap();
    // The first lane stalls past its lease; another claim takes the item over.
    let later = now + 130_000;
    heartbeat(&db, later);
    let GenClaimReply::Items { items: retaken, .. } = tx(&db, |s| lanes::gen_claim(s, &c, &b, "b", &grants, later)).unwrap() else { panic!() };
    assert_eq!(retaken[0].idx, first[0].idx);
    assert!(retaken[0].claim > first[0].claim);
    // The late result records its usage but is discarded.
    let late = outcome(&first[0], Ok(answer(&first[0])));
    let v = tx(&db, |s| lanes::gen_commit(s, &c, &a, &[late], later)).unwrap();
    assert_eq!(v["fenced"], 1);
    let (status, cost): (String, i64) = db
        .conn
        .query_row("SELECT d.status, c.gen_cost_microusd FROM docs d, cohorts c WHERE d.seq = 1 AND d.idx = ?1 AND c.seq = 1", [first[0].idx], |r| Ok((r.get(0)?, r.get(1)?)))
        .unwrap();
    assert_eq!(status, "pending");
    assert!(cost > 0, "the spend was real");
    let reserved: i64 = db.conn.query_row("SELECT gen_reserved_microusd FROM cohorts WHERE seq = 1", [], |r| r.get(0)).unwrap();
    assert!(reserved <= held, "a stale reservation is replaced, never added to");
}

#[test]
fn infrastructure_failures_past_max_attempts_extend_the_chunk_and_the_same_cohort_finishes() {
    let db = Sqlite::open();
    sql::migrate(&db, &EVAL_COHORT_SCHEMA).unwrap();
    let seq = open_cohort(&db, &wide(20, 2), 10, 1_000_000);
    let mut sim = Sim::new(&db, cfg(), 1_000_010);
    sim.tick_ms = 5_000;
    // Job 1's chunk 2 stores nothing for its first five attempts (lane
    // evictions, expired leases): every one of the plan's 3 attempts and two
    // more are lost to infrastructure. None of that ends the cohort.
    sim.score = Box::new(|q| if q.job == 1 && q.chunk == 2 && q.attempt <= 5 { Score::DieBeforePut } else { Score::Ok });
    assert_eq!(sim.run(seq, 4_000), "transition", "the same cohort finishes");
    assert_eq!(fault_of(&db, seq), None);
    let n = numbers(&db, seq, 1, 8);
    assert_eq!(n[..5], [(1, 5), (2, 5), (3, 5), (4, 5), (5, 5)], "every lost attempt has its receipt: {n:?}");
    assert_eq!(n[5].0, 6, "the first attempt that stored its results resolves it: {n:?}");
    assert_ne!(n[5].1, 5);
    let st = status(&db, sim.now);
    let c0 = &st["cohorts"][0];
    assert_eq!(c0["attempt_extensions"], 1, "one round of max_attempts more: {}", c0["log"]);
    assert_eq!(c0["extension_receipts"], 4 * 3, "the chunk's 4 unresolved documents at 3 added attempts");
    assert!(
        c0["log"].as_array().unwrap().iter().any(|e| e["note"].as_str().is_some_and(|n| n.contains("attempts extended to 6"))),
        "{}",
        c0["log"]
    );
    let b = sim.packages.pop().unwrap();
    verify(&sim, &b);
}

#[test]
fn a_chunk_with_no_further_attempt_faults_alone_and_a_clear_resumes_the_same_cohort() {
    let db = Sqlite::open();
    sql::migrate(&db, &EVAL_COHORT_SCHEMA).unwrap();
    let seq = open_cohort(&db, &wide(20, 2), 10, 1_000_000);
    let mut sim = Sim::new(&db, cfg(), 1_000_010);
    sim.tick_ms = 5_000;
    // The attempt number's range is the structural bound: a chunk already
    // allowed 255 attempts gets no further round. Model it by moving job 1's
    // chunk 2, before anything of it is recorded, 254 attempts on: its first
    // attempt (claimed or not) becomes the 255th.
    sim.score = Box::new(|q| if q.job == 1 && q.chunk == 2 { Score::DieBeforePut } else { Score::Ok });
    let mut moved = 0;
    for _ in 0..2_000 {
        moved = db
            .conn
            .execute(
                "UPDATE chunks SET attempt_limit = 255, attempt = attempt + 254, recorded = 254,
                    inflight = CASE WHEN inflight > 0 THEN inflight + 254 ELSE 0 END
                 WHERE seq = 1 AND job = 1 AND chunk = 2 AND attempt <= 1 AND recorded = 0",
                [],
            )
            .unwrap();
        if moved == 1 {
            break;
        }
        sim.tick();
    }
    assert_eq!(moved, 1, "the chunk was moved before its first claim");
    assert_eq!(sim.run(seq, 2_000), "scoring");
    let st = status(&db, sim.now);
    let c0 = &st["cohorts"][0];
    assert_eq!(c0["fault"], "unresolved_infrastructure");
    assert_eq!(c0["job_status"], json!({"done": 2, "faulted": 1}), "the other jobs finished");
    assert!(st["resumable_faults"].as_array().unwrap().iter().any(|f| f == "unresolved_infrastructure"));
    // Resumable: the clear returns the job to scoring. Its next claim still
    // finds no further attempt within the bound and faults it again, visibly.
    let cleared = tx(&db, |s| engine::clear_fault(s, "testnet-rehearsal-1", sim.now)).unwrap();
    assert_eq!((cleared.cleared, cleared.refused), (true, None));
    let js: String = db.conn.query_row("SELECT status FROM jobs WHERE seq = 1 AND job = 1", [], |r| r.get(0)).unwrap();
    assert_eq!(js, "running");
    assert_eq!(sim.run(seq, 400), "scoring");
    assert_eq!(fault_of(&db, seq).as_deref(), Some("unresolved_infrastructure"));
    // Abandonment stays available to the operator.
    assert!(tx(&db, |s| engine::abandon(s, "testnet-rehearsal-1", "unresolved_infrastructure", sim.now)).unwrap());
}

#[test]
fn an_extension_past_one_signed_receipt_index_faults_the_job_instead() {
    let db = Sqlite::open();
    sql::migrate(&db, &EVAL_COHORT_SCHEMA).unwrap();
    let p = wide(20, 2);
    let seq = open_cohort(&db, &p, 10, 1_000_000);
    // Earlier extensions already account for every receipt one signed index
    // holds beyond the plan's own reservation.
    let room = super::plan::receipt_index_bound() - p.receipt_bound();
    db.conn.execute("UPDATE cohorts SET extension_receipts = ?1 WHERE seq = 1", [room as i64]).unwrap();
    let mut sim = Sim::new(&db, cfg(), 1_000_010);
    sim.tick_ms = 5_000;
    sim.score = Box::new(|q| if q.job == 1 && q.chunk == 2 { Score::DieBeforePut } else { Score::Ok });
    sim.run(seq, 2_000);
    let c0 = &status(&db, sim.now)["cohorts"][0];
    assert_eq!(c0["fault"], "unresolved_infrastructure");
    assert_eq!(c0["attempt_extensions"], 0);
    assert_eq!(numbers(&db, seq, 1, 8), [(1, 5), (2, 5), (3, 5)], "every allowed attempt has a receipt");
}

#[test]
fn the_scoring_budget_alerts_and_never_pauses_the_cohort() {
    let db = Sqlite::open();
    sql::migrate(&db, &EVAL_COHORT_SCHEMA).unwrap();
    let mut p = wide(20, 1);
    let f = p.fanout.as_mut().unwrap();
    // Exactly the plan's own upper bound, so it is admitted.
    let ub = super::cost::nano_to_micro_ceil(super::cost::scoring_upper_bound_nanousd(2, 20, 6, &f.cost));
    f.score.scoring_budget_microusd = ub;
    let seq = open_cohort(&db, &p, 10, 1_000_000);
    let mut sim = Sim::new(&db, cfg(), 1_000_010);
    sim.claims_per_run = 1;
    for _ in 0..100 {
        sim.tick();
        let preds: i64 = db.conn.query_row("SELECT COUNT(*) FROM preds", [], |r| r.get(0)).unwrap();
        if preds > 0 {
            break;
        }
    }
    assert_eq!((phase(&db, seq).as_str(), fault_of(&db, seq)), ("scoring", None), "measured spend fits");
    assert_eq!(status(&db, sim.now)["cohorts"][0]["budget_alert"], Value::Null);
    // The lanes' metering now says far more was spent than the bound assumed.
    db.conn.execute("UPDATE lanes SET active_ms = 10000000 WHERE seq = 1 AND kind = 'score' AND first_seen_ms > 0", []).unwrap();
    sim.tick();
    // Spend is tracked, never capped: the alert is raised and nothing pauses.
    assert_eq!(fault_of(&db, seq), None);
    let c0 = &status(&db, sim.now)["cohorts"][0];
    assert_eq!(c0["budget_alert"], "scoring_budget");
    assert_eq!(
        c0["log"].as_array().unwrap().iter().filter(|e| e["note"].as_str().is_some_and(|n| n.starts_with("budget:"))).count(),
        1
    );
    assert_eq!(sim.run(seq, 400), "transition", "the same cohort finishes");
    assert_eq!(fault_of(&db, seq), None);
    let c0 = &status(&db, sim.now)["cohorts"][0];
    assert_eq!(c0["budget_alert"], "scoring_budget", "kept for the cohort");
    assert_eq!(
        c0["log"].as_array().unwrap().iter().filter(|e| e["note"].as_str().is_some_and(|n| n.starts_with("budget:"))).count(),
        1,
        "logged once"
    );
    // A store that paused under the earlier guard still resumes with a clear.
    db.conn.execute("UPDATE cohorts SET fault = 'scoring_budget' WHERE seq = 1", []).unwrap();
    assert!(tx(&db, |s| engine::clear_fault(s, &p.label, sim.now)).unwrap().cleared);
    // A plan whose budget is below its own upper bound is refused outright.
    let mut low = wide(20, 1);
    low.cohort = 9;
    low.label = "low".into();
    low.fanout.as_mut().unwrap().score.scoring_budget_microusd = ub - 1;
    assert_eq!(
        tx(&db, |s| engine::queue(s, &cfg(), &serde_json::to_vec(&low).unwrap(), 5)).unwrap_err().code(),
        "scoring budget is below the plan's upper bound"
    );
}

/// L2 (A2): a generation outage pauses the cohort; once the provider answers
/// again the next tick's probe resumes the same cohort with nothing reset,
/// with no operator. A probe that keeps failing only keeps the cohort
/// paused, however long, and every probe's time and spend are recorded
/// and charged with the cohort.
#[test]
fn a_generation_outage_resumes_by_itself_once_the_provider_answers() {
    let db = Sqlite::open();
    sql::migrate(&db, &EVAL_COHORT_SCHEMA).unwrap();
    let p = wide(20, 0);
    let seq = open_cohort(&db, &p, 10, 1_000_000);
    let mut sim = Sim::new(&db, cfg(), 1_000_010);
    sim.tick_ms = 10_000;
    sim.generate = Box::new(|_| Err(Reject::Transport));
    sim.run(seq, 400);
    assert_eq!(fault_of(&db, seq).as_deref(), Some("generation_unavailable"));
    // A day of failing probes: paused, never skipped or abandoned.
    for _ in 0..8_640 {
        sim.tick();
    }
    assert_eq!((phase(&db, seq).as_str(), fault_of(&db, seq).as_deref()), ("generating", Some("generation_unavailable")));
    let runs = sim.probes.len();
    assert!((1_400..=1_441).contains(&runs), "one probe a minute: {runs}");
    assert!(sim.probes.iter().all(|c| c == "generation_unavailable"));
    let st = status(&db, sim.now);
    let pr = &st["cohorts"][0]["probes"];
    assert_eq!((pr["runs"].as_u64(), pr["failures"].as_u64()), (Some(runs as u64), Some(runs as u64)));
    assert_eq!(pr["microusd"].as_u64(), Some(runs as u64 * 1_000), "the probes' spend is recorded");
    let failed: i64 = db.conn.query_row("SELECT SUM(status = 'failed') FROM docs WHERE seq = 1", [], |r| r.get(0)).unwrap();
    assert_eq!(failed, 0, "no item was dropped");
    // The provider is back: the next probe resumes it.
    sim.generate = Box::new(|it| Ok(answer(it)));
    for _ in 0..7 {
        sim.tick();
    }
    assert_eq!(fault_of(&db, seq), None);
    let log = st_log(&db, seq);
    assert_eq!(log.iter().filter(|n| n.starts_with("fault_auto_cleared:generation_unavailable")).count(), 1);
    assert_eq!(sim.run(seq, 400), "transition");
    // The probes are charged with the cohort's spend at its settle.
    for _ in 0..3 {
        sim.tick();
    }
    let (settled, gen_cost): (i64, i64) = db
        .conn
        .query_row("SELECT settled_microusd, gen_cost_microusd FROM cohorts WHERE seq = 1", [], |r| Ok((r.get(0)?, r.get(1)?)))
        .unwrap();
    assert!(settled >= gen_cost + (runs as i64 + 1) * 1_000, "{settled} {gen_cost}");
}

fn st_log(db: &Sqlite, seq: i64) -> Vec<String> {
    db.conn
        .prepare("SELECT note FROM phase_log WHERE seq = ?1 ORDER BY rowid")
        .unwrap()
        .query_map([seq], |r| r.get::<_, Option<String>>(0))
        .unwrap()
        .filter_map(Result::unwrap)
        .collect()
}

/// L2 (A3): scorer slots that never reach the model pause the cohort; once
/// the platform recovers, a probe (the control document on a fresh
/// instance of the reference) resumes the same cohort. The probe never
/// counts against an entrant's artifact. Terminal faults are never cleared
/// by a probe, and no probe runs while the coordinator is silent.
#[test]
fn a_scorer_outage_resumes_by_itself_and_terminal_faults_never_do() {
    let db = Sqlite::open();
    sql::migrate(&db, &EVAL_COHORT_SCHEMA).unwrap();
    let seq = open_cohort(&db, &wide(20, 2), 10, 1_000_000);
    let mut sim = Sim::new(&db, cfg(), 1_000_010);
    sim.tick_ms = 3_000;
    let down = std::rc::Rc::new(std::cell::Cell::new(true));
    let d2 = down.clone();
    sim.score = Box::new(move |_| if d2.get() { Score::NotDispatched } else { Score::Ok });
    sim.run(seq, 400);
    assert_eq!(fault_of(&db, seq).as_deref(), Some("scorer_unavailable"));
    for _ in 0..100 {
        sim.tick();
    }
    assert_eq!(fault_of(&db, seq).as_deref(), Some("scorer_unavailable"), "the platform is still down");
    assert!(!sim.probes.is_empty());
    // Silence: once the last heartbeat is stale, no probe runs.
    sim.heartbeats = false;
    for _ in 0..=(engine::HEARTBEAT_FRESH_MS / sim.tick_ms) {
        sim.tick();
    }
    let n = sim.probes.len();
    for _ in 0..400 {
        sim.tick();
    }
    assert_eq!(sim.probes.len(), n, "no probe while the coordinator is silent");
    sim.heartbeats = true;
    down.set(false);
    for _ in 0..25 {
        sim.tick();
    }
    assert_eq!(fault_of(&db, seq), None, "the first good probe resumes it");
    assert_eq!(sim.run(seq, 400), "transition");
    let failures: i64 = db.conn.query_row("SELECT SUM(artifact_failures) FROM jobs WHERE seq = 1", [], |r| r.get(0)).unwrap();
    assert_eq!(failures, 0, "no entrant was charged for a probe or an outage");
    // A terminal fault: probes never run for it and never clear it.
    let db = Sqlite::open();
    sql::migrate(&db, &EVAL_COHORT_SCHEMA).unwrap();
    let seq = open_cohort(&db, &wide(20, 0), 10, 1_000_000);
    let mut sim = Sim::new(&db, cfg(), 1_000_010);
    sim.tick();
    assert_eq!(phase(&db, seq), "generating");
    tx(&db, |s| engine::fault(s, seq, "batch_shortfall", sim.now)).unwrap();
    for _ in 0..200 {
        sim.tick();
    }
    assert!(sim.probes.is_empty());
    assert_eq!(fault_of(&db, seq).as_deref(), Some("batch_shortfall"));
    assert!(!tx(&db, |s| engine::auto_clear(s, seq, "batch_shortfall", "probe", sim.now)).unwrap());
    let ok = engine::ProbeOutcome { ok: true, definitive: false, ms: 1, nanousd: 1, note: "ok".into() };
    let ticket = engine::ProbeTicket { started_ms: sim.now, fault_n: 1, until_ms: sim.now };
    assert!(!tx(&db, |s| engine::probe_done(s, seq, "batch_shortfall", &ticket, &ok, sim.now)).unwrap());
    assert_eq!(fault_of(&db, seq).as_deref(), Some("batch_shortfall"));
    let st = status(&db, sim.now);
    assert_eq!((st["cohorts"][0]["self_clearing"].as_bool(), st["cohorts"][0]["operator_clear_needed"].as_bool()), (Some(false), Some(true)));
}

/// L2 (B8, C1 item 3, eval half): abandoning a cohort writes its signed,
/// content-free abandonment record beside its other signed records, once,
/// so the automatic target handoff can move past it.
#[test]
fn an_abandoned_cohort_gets_one_signed_abandonment_record() {
    let db = Sqlite::open();
    sql::migrate(&db, &EVAL_COHORT_SCHEMA).unwrap();
    let p = wide(20, 0);
    let seq = open_cohort(&db, &p, 10, 1_000_000);
    let mut sim = Sim::new(&db, cfg(), 1_000_010);
    sim.generate = Box::new(|_| Err(Reject::Transport));
    sim.tick_ms = 10_000;
    sim.run(seq, 400);
    assert!(tx(&db, |s| engine::abandon(s, &p.label, "drill", sim.now)).unwrap());
    sim.tick();
    let key = format!("cohorts/{}/{}", p.label, engine::ABANDONMENT_KEY);
    let bytes = sim.rcpt.get(&key).expect("the record is written").clone();
    let keys = slop_ninja_protocol::fixtures::trusted_keys();
    let r = slop_ninja_protocol::Signed::<slop_ninja_protocol::submission::CohortAbandonment>::open(&bytes, &keys).unwrap();
    assert_eq!(
        (r.cohort, r.cohort_label.as_str(), r.cohort_manifest, r.fault.as_str(), r.reason.as_str()),
        (p.cohort, p.label.as_str(), None, "generation_unavailable", "drill")
    );
    assert_eq!(r.chain, cfg().chain);
    assert!(r.abandoned_epoch.is_some());
    assert!(status(&db, sim.now)["cohorts"][0]["abandonment_record_ms"].as_u64().is_some());
    sim.rcpt.remove(&key);
    for _ in 0..5 {
        sim.tick();
    }
    assert!(!sim.rcpt.contains_key(&key), "written once");
    assert!(tx(&db, |s| engine::abandon_marker_due(s, &cfg(), None)).unwrap().is_none());
    assert!(tx(&db, |s| engine::abandon_marker_due(s, &load_cfg(), None)).unwrap().is_none());
}

/// L2 (B8 item 2): a reference is never attributed. A document that
/// crashes the reference's instance even alone on a proven instance is an
/// infrastructure error, not a model error that would hold the package for
/// good (`reference_failure`): it stays unresolved and runs again, past the
/// plan's `max_attempts` (`lanes::extend_attempts`), and the same cohort
/// finishes once the platform answers. No other document runs again.
#[test]
fn a_reference_document_that_keeps_crashing_stays_unresolved_and_reruns_alone() {
    let db = Sqlite::open();
    sql::migrate(&db, &EVAL_COHORT_SCHEMA).unwrap();
    let seq = open_cohort(&db, &wide(20, 1), 10, 1_000_000);
    let mut sim = Sim::new(&db, cfg(), 1_000_010);
    sim.tick_ms = 5_000;
    // Task 3 kills the reference's instance (alone too) through attempt 4.
    sim.score = Box::new(|q| if q.job == 0 && q.tasks.contains(&3) && q.attempt <= 4 { Score::Crash } else { Score::Ok });
    assert_eq!(sim.run(seq, 3_000), "transition");
    let outcomes = numbers(&db, seq, 0, 3);
    assert!(outcomes.len() > 3, "past max_attempts: {outcomes:?}");
    assert_eq!(outcomes.last().map(|x| x.1), Some(1), "answered at last");
    // Before it: infrastructure receipts (the chunk's crash) and the model
    // errors the scorer gave alone (the isolated crash), which the receipts
    // record as they were but which never resolve a reference's document.
    assert!(outcomes[..outcomes.len() - 1].iter().all(|(_, o)| *o == 5 || *o == 2), "unresolved before: {outcomes:?}");
    assert!(outcomes.iter().any(|(_, o)| *o == 2), "the receipts keep the scorer's model error: {outcomes:?}");
    let model_errors: i64 =
        db.conn.query_row("SELECT COUNT(*) FROM preds WHERE seq = 1 AND job = 0 AND outcome = 2", [], |r| r.get(0)).unwrap();
    assert_eq!(model_errors, 0, "a reference is never attributed a model error");
    for t in (0..20).filter(|t| !(0..4).contains(t)) {
        assert_eq!(numbers(&db, seq, 0, t), [(1, 1)], "task {t} ran once");
    }
}

/// L2 review r1 (item 1): a `scorer_unavailable` whose cause the probe
/// does not exercise (bucket writes that fail) is recorded with its cause
/// and never cleared by a probe: the cohort stays paused and visible until
/// an operator clears it, with no fault/clear loop.
#[test]
fn a_store_caused_scorer_pause_is_never_cleared_by_a_probe() {
    let db = Sqlite::open();
    sql::migrate(&db, &EVAL_COHORT_SCHEMA).unwrap();
    let seq = open_cohort(&db, &wide(20, 1), 10, 1_000_000);
    let mut sim = Sim::new(&db, cfg(), 1_000_010);
    sim.tick_ms = 3_000;
    let outage = std::rc::Rc::new(std::cell::Cell::new(true));
    let o2 = outage.clone();
    // Every results and receipt object write fails; the scorer is fine.
    sim.r2 = Box::new(move |key, put| o2.get() && put && !key.ends_with(engine::ABANDONMENT_KEY));
    sim.run(seq, 400);
    assert_eq!(fault_of(&db, seq).as_deref(), Some("scorer_unavailable"));
    for _ in 0..400 {
        sim.tick();
    }
    assert!(sim.probes.is_empty(), "the probe touches no bucket, so none runs: {:?}", sim.probes);
    let st = status(&db, sim.now);
    assert_eq!(st["cohorts"][0]["fault_cause"], "store");
    assert_eq!(st["cohorts"][0]["probes"]["probed"], false);
    let log = st_log(&db, seq);
    assert_eq!(log.iter().filter(|n| n.starts_with("fault:")).count(), 1, "one pause, no loop: {log:?}");
    assert!(!log.iter().any(|n| n.starts_with("fault_auto_cleared")));
    outage.set(false);
    assert!(tx(&db, |s| engine::clear_fault(s, "testnet-rehearsal-1", sim.now)).unwrap().cleared);
    assert_eq!(sim.run(seq, 800), "transition");
}

/// L2 review r2 (item 3): a probe that succeeds while capacity is only
/// partly back clears the pause, and the cohort pausing again before any
/// chunk commits is probed again after a doubled interval each time (no
/// tight loop, and never held for an operator). Once capacity fully
/// returns, the same cohort resumes and finishes with no operator, and its
/// progress resets the back-off.
#[test]
fn a_partly_recovered_scorer_backs_the_probe_off_and_the_cohort_still_resumes_by_itself() {
    let db = Sqlite::open();
    sql::migrate(&db, &EVAL_COHORT_SCHEMA).unwrap();
    let seq = open_cohort(&db, &wide(20, 1), 10, 1_000_000);
    let mut sim = Sim::new(&db, cfg(), 1_000_010);
    sim.tick_ms = 3_000;
    let down = std::rc::Rc::new(std::cell::Cell::new(true));
    let d2 = down.clone();
    // The probe's single instance scores its control document; the lanes'
    // batches still find no capacity.
    sim.score = Box::new(move |q| if d2.get() && q.chunk >= 0 { Score::NotDispatched } else { Score::Ok });
    sim.run(seq, 400);
    assert_eq!(fault_of(&db, seq).as_deref(), Some("scorer_unavailable"));
    let t0 = sim.now;
    while sim.now < t0 + 30 * 60_000 {
        sim.tick();
    }
    // Pause at 0, then probes at about 2, 6, 14 and 30 minutes.
    let runs = sim.probes.len();
    assert!((3..=6).contains(&runs), "backed off, never a loop and never held: {runs}");
    let log = st_log(&db, seq);
    assert_eq!(log.iter().filter(|n| n.starts_with("fault_auto_cleared")).count(), runs, "{log:?}");
    let st = status(&db, sim.now);
    let pr = &st["cohorts"][0]["probes"];
    assert!(pr["no_progress_clears"].as_i64().unwrap() >= 3, "{pr}");
    assert!(pr["interval_ms"].as_u64().unwrap() >= 8 * engine::PROBE_EVERY_MS, "{pr}");
    if st["cohorts"][0]["fault"].is_string() {
        assert_eq!(st["cohorts"][0]["self_clearing"], true);
        assert_eq!(st["cohorts"][0]["operator_clear_needed"], false);
    }
    // Capacity fully returns: the same cohort finishes with no operator.
    down.set(false);
    let mut finished = false;
    for _ in 0..3_000 {
        sim.tick();
        if phase(&db, seq) == "transition" {
            finished = true;
            break;
        }
    }
    assert!(finished, "resumed by itself: {}", phase(&db, seq));
    assert!(!st_log(&db, seq).iter().any(|n| n == "fault_cleared"), "no operator clear");
    let cleared: i64 = db.conn.query_row("SELECT auto_cleared FROM cohorts WHERE seq = ?1", [seq], |r| r.get(0)).unwrap();
    assert_eq!(cleared, 0, "progress reset the back-off");
}

/// L2 review r1 (item 1), r2 (item 3): an automatic clear keeps the next
/// probe at least a backed-off interval away, so a cohort that pauses again
/// within seconds is not probed again before it; an operator clear probes
/// at once.
#[test]
fn an_automatic_clear_keeps_the_next_probe_time() {
    let db = Sqlite::open();
    sql::migrate(&db, &EVAL_COHORT_SCHEMA).unwrap();
    let seq = open_cohort(&db, &wide(20, 0), 10, 1_000_000);
    let now = 2_000_000;
    tx(&db, |s| engine::fault(s, seq, engine::GENERATION_UNAVAILABLE, now)).unwrap();
    let ok = engine::ProbeOutcome { ok: true, definitive: false, ms: 1, nanousd: 1, note: "ok".into() };
    let probe = engine::Probe::Scorer { lane: "x".into(), body: json!({}), admit: false };
    let t = tx(&db, |s| engine::probe_begin(s, seq, &probe, now)).unwrap();
    assert!(tx(&db, |s| engine::probe_done(s, seq, engine::GENERATION_UNAVAILABLE, &t, &ok, now + 5)).unwrap());
    let after: i64 = db.conn.query_row("SELECT probe_after_ms FROM cohorts WHERE seq = ?1", [seq], |r| r.get(0)).unwrap();
    assert_eq!(after as u64, now + 5 + engine::probe_interval_ms(1), "one clear without progress: a doubled interval");
    assert_eq!(engine::probe_interval_ms(1), 2 * engine::PROBE_EVERY_MS);
    assert_eq!(engine::probe_interval_ms(i64::MAX), engine::PROBE_EVERY_MS << engine::PROBE_BACKOFF_DOUBLINGS);
    // An operator clear probes at once.
    tx(&db, |s| engine::fault(s, seq, engine::GENERATION_UNAVAILABLE, now + 10)).unwrap();
    assert!(tx(&db, |s| engine::clear_fault(s, "testnet-rehearsal-1", now + 20)).unwrap().cleared);
    let after: i64 = db.conn.query_row("SELECT probe_after_ms FROM cohorts WHERE seq = ?1", [seq], |r| r.get(0)).unwrap();
    assert_eq!(after, 0);
}

/// L2 review r1 (item 5), r2 (item 7): a probe's start is durable before
/// its call. While it may still run (its deadline), no second probe starts,
/// however many steps run; a probe cut off mid-call is charged its bound
/// once its deadline and an interval have passed, and the cut-off probe's
/// late outcome leaves the newer probe's pending record alone.
#[test]
fn a_probe_cut_off_mid_call_is_charged_its_bound() {
    let db = Sqlite::open();
    sql::migrate(&db, &EVAL_COHORT_SCHEMA).unwrap();
    let seq = open_cohort(&db, &wide(20, 0), 10, 1_000_000);
    let now = 2_000_000;
    tx(&db, |s| engine::fault(s, seq, engine::GENERATION_UNAVAILABLE, now)).unwrap();
    db.conn.execute("UPDATE eval_obs SET kick_ms = ?1 WHERE id = 1", [now as i64]).unwrap();
    let Some(Work::Probe { probe, .. }) = tx(&db, |s| engine::probe_due(s, &cfg(), now)).unwrap() else {
        panic!("a probe is due");
    };
    let c = tx(&db, |s| engine::cohort(s, seq)).unwrap();
    let bound = engine::probe_bound_nanousd(&c, &probe).unwrap();
    assert!(bound > 0);
    let first = tx(&db, |s| engine::probe_begin(s, seq, &probe, now)).unwrap();
    assert_eq!(first.until_ms, now + engine::probe_run_ms(&c, &probe).unwrap() + crate::liveness::TIMEOUT_MS);
    // Steps while it runs, past the old one-minute interval: none starts another.
    for at in [now + 1, now + engine::PROBE_EVERY_MS + 1, first.until_ms - 1, first.until_ms + engine::PROBE_EVERY_MS - 1] {
        db.conn.execute("UPDATE eval_obs SET kick_ms = ?1 WHERE id = 1", [at as i64]).unwrap();
        assert!(tx(&db, |s| engine::probe_due(s, &cfg(), at)).unwrap().is_none(), "in flight at {at}");
    }
    // The step was cut off: the next probe charges the first one's bound.
    let later = first.until_ms + engine::PROBE_EVERY_MS;
    db.conn.execute("UPDATE eval_obs SET kick_ms = ?1 WHERE id = 1", [later as i64]).unwrap();
    assert!(tx(&db, |s| engine::probe_due(s, &cfg(), later)).unwrap().is_some());
    let second = tx(&db, |s| engine::probe_begin(s, seq, &probe, later)).unwrap();
    let (charged, runs, last): (i64, i64, String) = db
        .conn
        .query_row("SELECT probe_nanousd, probes, probe_last FROM cohorts WHERE seq = ?1", [seq], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
        .unwrap();
    assert_eq!((charged as u64, runs, last.as_str()), (bound, 1, "cut_off"));
    let st = status(&db, later);
    assert_eq!(st["cohorts"][0]["probes"]["pending_bound_microusd"].as_u64(), Some(bound.div_ceil(1000)));
    // The first probe's late outcome: the second's record stays.
    let fail = engine::ProbeOutcome { ok: false, definitive: false, ms: 7, nanousd: 9, note: "late".into() };
    assert!(!tx(&db, |s| engine::probe_done(s, seq, engine::GENERATION_UNAVAILABLE, &first, &fail, later + 1)).unwrap());
    let (started, pending): (Option<i64>, i64) = db
        .conn
        .query_row("SELECT probe_started_ms, probe_pending_nanousd FROM cohorts WHERE seq = ?1", [seq], |r| Ok((r.get(0)?, r.get(1)?)))
        .unwrap();
    assert_eq!((started.map(|v| v as u64), pending as u64), (Some(second.started_ms), bound));
    // The second's own outcome is recorded and ends it.
    let ok = engine::ProbeOutcome { ok: true, definitive: false, ms: 1, nanousd: 1, note: "ok".into() };
    assert!(tx(&db, |s| engine::probe_done(s, seq, engine::GENERATION_UNAVAILABLE, &second, &ok, later + 2)).unwrap());
}

/// L2 review r2 (item 6): a probe's outcome applies only to the fault it was
/// started for. An operator clears the fault while a probe runs and the
/// cohort pauses again with the same code: the old probe's success does not
/// resume the new pause, and its definitive refusal does not hold it.
#[test]
fn a_probe_outcome_never_applies_to_a_later_fault_with_the_same_code() {
    let db = Sqlite::open();
    sql::migrate(&db, &EVAL_COHORT_SCHEMA).unwrap();
    let seq = open_cohort(&db, &wide(20, 1), 10, 1_000_000);
    let now = 2_000_000;
    tx(&db, |s| engine::fault_with_cause(s, seq, engine::SCORER_UNAVAILABLE, Some("slot"), now)).unwrap();
    let probe = engine::Probe::Scorer { lane: "x".into(), body: json!({}), admit: false };
    for o in [
        engine::ProbeOutcome { ok: true, definitive: false, ms: 1, nanousd: 1, note: "ok".into() },
        engine::ProbeOutcome { ok: false, definitive: true, ms: 1, nanousd: 1, note: "definitive_refusal:x".into() },
    ] {
        let t = tx(&db, |s| engine::probe_begin(s, seq, &probe, now)).unwrap();
        assert!(tx(&db, |s| engine::clear_fault(s, "testnet-rehearsal-1", now + 1)).unwrap().cleared);
        tx(&db, |s| engine::fault_with_cause(s, seq, engine::SCORER_UNAVAILABLE, Some("slot"), now + 2)).unwrap();
        assert!(!tx(&db, |s| engine::probe_done(s, seq, engine::SCORER_UNAVAILABLE, &t, &o, now + 3)).unwrap());
        assert_eq!(fault_of(&db, seq).as_deref(), Some("scorer_unavailable"), "the new pause stays");
        assert_eq!(status(&db, now + 3)["cohorts"][0]["fault_cause"], "slot", "and keeps its own cause");
    }
}

/// L2 review r2 (item 1): a probe that takes minutes never holds the
/// bookkeeping of an earlier cohort while a later one stays paused: the
/// purge of private predictions, the spend settle and the signed
/// abandonment record all run between probes. A ledger that does not
/// answer only delays the settle, which waits out its retry time, so it
/// never holds the probe either.
#[test]
fn long_probes_never_hold_the_purge_settle_or_abandonment_record() {
    let db = Sqlite::open();
    sql::migrate(&db, &EVAL_COHORT_SCHEMA).unwrap();
    let p = wide(20, 0);
    let seq = open_cohort(&db, &p, 10, 1_000_000);
    let mut sim = Sim::new(&db, cfg(), 1_000_010);
    sim.tick_ms = 10_000;
    sim.generate = Box::new(|_| Err(Reject::Transport));
    sim.run(seq, 400);
    assert!(tx(&db, |s| engine::abandon(s, &p.label, "drill", sim.now)).unwrap());
    // The next cohort opens and pauses on the same outage before any tick
    // reaches the earlier cohort's bookkeeping.
    let mut p2 = wide(20, 0);
    p2.cohort = 2;
    p2.label = "testnet-rehearsal-2".into();
    let seq2 = tx(&db, |s| engine::queue(s, &cfg(), &serde_json::to_vec(&p2).unwrap(), sim.now)).unwrap();
    for e in 0..3 {
        kick_at(&db, 20 + e, sim.now);
    }
    sim.ledger_down = true;
    sim.probe_ms = 120_000;
    let purged = |db: &Sqlite| -> Option<i64> { db.conn.query_row("SELECT purged_ms FROM cohorts WHERE seq = 1", [], |r| r.get(0)).unwrap() };
    let settled = |db: &Sqlite| -> Option<i64> { db.conn.query_row("SELECT settled_microusd FROM cohorts WHERE seq = 1", [], |r| r.get(0)).unwrap() };
    let mut paused_at = None;
    for _ in 0..400 {
        sim.tick();
        if paused_at.is_none() && fault_of(&db, seq2).is_some() {
            paused_at = Some(sim.probes.len());
        }
        if paused_at.is_some() && sim.probes.len() >= paused_at.unwrap() + 4 {
            break;
        }
    }
    assert_eq!(fault_of(&db, seq2).as_deref(), Some("generation_unavailable"), "the later cohort stays paused");
    assert!(sim.probes.len() >= paused_at.unwrap() + 4, "probes kept running while the ledger failed: {:?}", sim.probes.len());
    assert!(purged(&db).is_some(), "private predictions purged between probes");
    // The purge ran before the later cohort's first probe, not behind it.
    let first = sim.probe_starts.iter().find(|(s, _)| *s == seq2).unwrap().1;
    assert!(purged(&db).unwrap() as u64 <= first, "purged at {:?}, first probe at {first}", purged(&db));
    let key = format!("cohorts/{}/{}", p.label, engine::ABANDONMENT_KEY);
    assert!(sim.rcpt.contains_key(&key), "the abandonment record was written between probes");
    assert!(settled(&db).is_none() && sim.settle_calls > 1, "the settle failed and was retried");
    let st = status(&db, sim.now);
    assert_eq!(st["bookkeeping_retry"][0]["kind"], "settle");
    // The ledger answers: the settle lands between probes.
    sim.ledger_down = false;
    for _ in 0..20 {
        sim.tick();
    }
    assert!(settled(&db).is_some());
    assert_eq!(fault_of(&db, seq2).as_deref(), Some("generation_unavailable"));
    assert!(status(&db, sim.now)["bookkeeping_retry"].as_array().unwrap().is_empty());
}

/// L2 review r2 (item 9): a cohort paused by a legacy budget fault signs
/// its abandonment record with the neutral `resource_guard` code, as both
/// its fault and (when the operator gave the fault code) its reason.
#[test]
fn a_budget_paused_cohort_is_abandoned_under_a_neutral_code() {
    let db = Sqlite::open();
    sql::migrate(&db, &EVAL_COHORT_SCHEMA).unwrap();
    let p = wide(20, 0);
    let seq = open_cohort(&db, &p, 10, 1_000_000);
    let mut sim = Sim::new(&db, cfg(), 1_000_010);
    sim.tick();
    db.conn.execute("UPDATE cohorts SET fault = 'scoring_budget' WHERE seq = ?1", [seq]).unwrap();
    assert!(tx(&db, |s| engine::abandon(s, &p.label, engine::SCORING_BUDGET, sim.now)).unwrap());
    let (_, r) = tx(&db, |s| engine::abandonment_record(s, &cfg(), seq)).unwrap();
    assert_eq!((r.fault.as_str(), r.reason.as_str()), (engine::RESOURCE_GUARD, engine::RESOURCE_GUARD));
    assert!(!r.fault.contains("budget") && !r.reason.contains("budget"));
    for _ in 0..5 {
        sim.tick();
    }
    let bytes = sim.rcpt[&format!("cohorts/{}/{}", p.label, engine::ABANDONMENT_KEY)].clone();
    let keys = slop_ninja_protocol::fixtures::trusted_keys();
    let signed = slop_ninja_protocol::Signed::<slop_ninja_protocol::submission::CohortAbandonment>::open(&bytes, &keys).unwrap();
    assert_eq!((signed.fault.as_str(), signed.reason.as_str()), ("resource_guard", "resource_guard"));
    assert_eq!(engine::ABANDONMENT_KEY, slop_ninja_admission_wire::receipts::ABANDONMENT_RECORD);
}

/// L2 review r1 (item 1): a previous reference's slot failures are probed
/// with its own artifact, not the reference's.
#[test]
fn a_previous_reference_pause_probes_its_own_artifact() {
    let db = Sqlite::open();
    sql::migrate(&db, &EVAL_COHORT_SCHEMA).unwrap();
    let mut p = wide(20, 1);
    p.previous_reference = Some(super::plan::tests::artifact(super::plan::tests::identity(0, 0xb0), 0x23));
    let seq = open_cohort(&db, &p, 10, 1_000_000);
    let mut sim = Sim::new(&db, cfg(), 1_000_010);
    sim.tick_ms = 3_000;
    let broken = std::rc::Rc::new(std::cell::Cell::new(true));
    let b2 = broken.clone();
    sim.start = Box::new(move |job, _| {
        if job == 1 && b2.get() { Err(RestartFailure::Artifact { code: "artifact load failed", definitive: false }) } else { Ok(()) }
    });
    sim.run(seq, 800);
    assert_eq!(fault_of(&db, seq).as_deref(), Some("scorer_unavailable"));
    assert_eq!(status(&db, sim.now)["cohorts"][0]["fault_cause"], "slot:previous_reference");
    for _ in 0..100 {
        sim.tick();
    }
    assert!(!sim.probe_jobs.is_empty());
    assert!(sim.probe_jobs.iter().all(|j| *j == 1), "the previous reference's artifact: {:?}", sim.probe_jobs);
    assert_eq!(fault_of(&db, seq).as_deref(), Some("scorer_unavailable"), "its load still fails");
    broken.set(false);
    for _ in 0..25 {
        sim.tick();
    }
    assert_eq!(fault_of(&db, seq), None, "its first good probe resumes it");
    assert_eq!(sim.run(seq, 800), "transition");
}

/// L2 review r1 (item 10): a generator the provider stopped serving
/// answers the probe as served (the work path rotates the item's
/// generator), so the paused cohort resumes instead of waiting.
#[test]
fn a_retired_generator_resumes_a_generation_pause() {
    let g = |m: Reject| GenOutcome { idx: 0, generator: "g".into(), claim: 0, output: Err(m), prompt_tokens: 0, completion_tokens: 0, ms: 0 };
    assert!(engine::probe_answered(&g(Reject::ModelUnavailable)));
    assert!(!engine::probe_answered(&g(Reject::Transport)));
    assert!(!engine::probe_answered(&g(Reject::Throttled)));
    let db = Sqlite::open();
    sql::migrate(&db, &EVAL_COHORT_SCHEMA).unwrap();
    let mut p = wide(20, 0);
    let (old, new) = ("@cf/meta/llama-3.2-3b-instruct", "@cf/meta/llama-3.2-1b-instruct");
    p.generators = vec![old.into(), new.into()];
    let seq = open_cohort(&db, &p, 10, 1_000_000);
    let mut sim = Sim::new(&db, cfg(), 1_000_010);
    sim.tick_ms = 10_000;
    sim.generate = Box::new(|_| Err(Reject::Transport));
    sim.run(seq, 400);
    assert_eq!(fault_of(&db, seq).as_deref(), Some("generation_unavailable"));
    // The old generator is retired; the other serves.
    sim.generate = Box::new(move |it| if it.generator == old { Err(Reject::ModelUnavailable) } else { Ok(answer(it)) });
    for _ in 0..12 {
        sim.tick();
    }
    assert_eq!(fault_of(&db, seq), None);
    assert_eq!(sim.run(seq, 800), "transition");
}

/// L2 review r1 (items 3, 11): an abandonment record that cannot be written
/// waits out its retry time and never holds up the next cohort's work.
#[test]
fn a_failing_abandonment_record_holds_no_other_work() {
    let db = Sqlite::open();
    sql::migrate(&db, &EVAL_COHORT_SCHEMA).unwrap();
    let p = wide(20, 0);
    let seq = open_cohort(&db, &p, 10, 1_000_000);
    let mut sim = Sim::new(&db, cfg(), 1_000_010);
    sim.tick_ms = 10_000;
    sim.generate = Box::new(|_| Err(Reject::Transport));
    sim.run(seq, 400);
    let blocked = std::rc::Rc::new(std::cell::Cell::new(true));
    let b2 = blocked.clone();
    sim.r2 = Box::new(move |key, put| b2.get() && put && key.ends_with(engine::ABANDONMENT_KEY));
    assert!(tx(&db, |s| engine::abandon(s, &p.label, engine::GENERATION_UNAVAILABLE, sim.now)).unwrap());
    sim.generate = Box::new(|it| Ok(answer(it)));
    let mut p2 = wide(20, 0);
    p2.cohort = 2;
    p2.label = "testnet-rehearsal-2".into();
    let bytes = serde_json::to_vec(&p2).unwrap();
    let seq2 = tx(&db, |s| engine::queue(s, &cfg(), &bytes, sim.now)).unwrap();
    for e in 0..3 {
        kick_at(&db, 20 + e, sim.now);
    }
    assert_eq!(sim.run(seq2, 2_000), "transition", "the next cohort ran while the record kept failing");
    let st = status(&db, sim.now);
    assert!(st["cohorts"][0]["abandonment_record_ms"].is_null());
    assert_eq!(st["cohorts"][0]["abandonment_record_retry"]["error"], "abandonment record put");
    blocked.set(false);
    for _ in 0..10 {
        sim.tick();
    }
    let key = format!("cohorts/{}/{}", p.label, engine::ABANDONMENT_KEY);
    assert!(sim.rcpt.contains_key(&key), "written once the bucket answers");
}

/// L2 review r1 (item 14): an abandonment reason is a listed public-safe
/// code or the cohort's own fault code; an older stored reason outside the
/// vocabulary is published as `unlisted`.
#[test]
fn abandonment_reasons_come_from_a_fixed_vocabulary() {
    let db = Sqlite::open();
    sql::migrate(&db, &EVAL_COHORT_SCHEMA).unwrap();
    let p = wide(20, 0);
    let seq = open_cohort(&db, &p, 10, 1_000_000);
    tx(&db, |s| engine::fault(s, seq, "batch_shortfall", 1_000_100)).unwrap();
    for bad in ["operator:spend_cap", "budget", "provider_account"] {
        assert!(tx(&db, |s| engine::abandon(s, &p.label, bad, 1_000_200)).is_err(), "{bad}");
    }
    assert!(tx(&db, |s| engine::abandon(s, &p.label, "batch_shortfall", 1_000_200)).unwrap());
    let (_, r) = tx(&db, |s| engine::abandonment_record(s, &cfg(), seq)).unwrap();
    assert_eq!(r.reason, "batch_shortfall");
    db.conn.execute("UPDATE cohorts SET abandoned_reason = 'old_free_code' WHERE seq = ?1", [seq]).unwrap();
    let (_, r) = tx(&db, |s| engine::abandonment_record(s, &cfg(), seq)).unwrap();
    assert_eq!(r.reason, "unlisted");
    for code in engine::ABANDON_REASONS {
        assert!(code.bytes().all(|b| b.is_ascii_lowercase() || b == b'_'), "{code}");
    }
}

/// L2 review r1 (item 9): a new cohort plan declares its generation pause
/// bound; a template without one gives its cohorts the compiled bound
/// written into their plans, so the seal binds it.
#[test]
fn a_new_plan_declares_its_generation_pause_bound() {
    let mut p = wide(20, 0);
    p.fanout.as_mut().unwrap().generation.failures_before_pause = None;
    let bytes = serde_json::to_vec(&p).unwrap();
    let e = engine::admit(&cfg(), &bytes).unwrap_err();
    assert!(format!("{e:?}").contains("fanout.generation.failures_before_pause"), "{e:?}");
    p.fanout.as_mut().unwrap().generation.failures_before_pause = Some(7);
    let (plan, _) = engine::admit(&cfg(), &serde_json::to_vec(&p).unwrap()).unwrap();
    assert_eq!(plan.fanout().unwrap().generation.failures_before_pause(), 7);
}

/// L2 review r1 (item 12): probes run on one stable lane object per
/// instance and authority, never one per cohort.
#[test]
fn the_probe_lane_is_one_stable_name_per_scope() {
    let name = engine::probe_lane_name(&cfg());
    assert_eq!(name, format!("a-scorer/{}/{}/fault-probe", crate::config::NETWORK, cfg().lane_scope()));
    assert!(!name.contains("rehearsal"));
}

#[test]
fn load_mode_rescores_a_sealed_batch_without_a_target() {
    // A main cohort seals a batch; the load instance re-scores it for 6 copies.
    let db = Sqlite::open();
    sql::migrate(&db, &EVAL_COHORT_SCHEMA).unwrap();
    let seq = open_cohort(&db, &wide(20, 0), 10, 1_000_000);
    let mut sim = Sim::new(&db, cfg(), 1_000_010);
    assert_eq!(sim.run(seq, 400), "transition");
    let index_key = "cohorts/testnet-rehearsal-1/batch/index.json".to_owned();
    let index = sim.eval[&index_key].clone();
    let load_db = Sqlite::open();
    sql::migrate(&load_db, &EVAL_COHORT_SCHEMA).unwrap();
    let base = plan(20);
    let lp = super::plan::LoadPlan {
        schema: super::plan::LOAD_PLAN_SCHEMA.into(),
        cohort: 1001,
        label: "load-copies".into(),
        source: super::plan::LoadSource { label: "testnet-rehearsal-1".into(), index_key: index_key.clone(),
            index_sha256: lane_logic::sha_hex(&index), tasks: 20 },
        max_attempts: 3,
        burn_identity: base.burn_identity.clone(),
        allow_list: (1..=6).map(|u| super::plan::tests::identity(u, u as u8)).collect(),
        reference: base.reference.clone(),
        entrants: (1..=6).map(|u| super::plan::tests::artifact(super::plan::tests::identity(u, u as u8), 0x22)).collect(),
        scorer: super::plan::Scorer { chunk_docs: 4, parallel_jobs: None },
        fanout: super::plan::tests::fanout(8),
        faults: Default::default(),
    };
    let bytes = serde_json::to_vec(&lp).unwrap();
    assert!(tx(&load_db, |s| engine::queue(s, &cfg(), &bytes, 5)).is_err(), "the main instance refuses load plans");
    heartbeat(&load_db, sim.now);
    let lseq = tx(&load_db, |s| engine::queue(s, &load_cfg(), &bytes, sim.now)).unwrap();
    assert_eq!(phase(&load_db, lseq), "sealing", "no boundary wait");
    let mut load = Sim::new(&load_db, load_cfg(), sim.now);
    load.eval = sim.eval.clone();
    // The load lanes read the source's private shards; the docs hold no text.
    let texts: i64 = load_db.conn.query_row("SELECT COUNT(*) FROM docs WHERE text IS NOT NULL", [], |r| r.get(0)).unwrap();
    assert_eq!(texts, 0);
    // The source's labels come from its text-free index; fake_results reads kinds from docs.
    assert_eq!(load.run(lseq, 400), "closed");
    let b = load.packages.pop().unwrap();
    assert_eq!(verify(&load, &b).len(), 7 * 20);
    assert_eq!(b.target.predecessor, None);
    let targets: i64 = load_db.conn.query_row("SELECT COUNT(*) FROM eval_targets", [], |r| r.get(0)).unwrap();
    assert_eq!(targets, 0, "a load cohort never activates a target");
    assert_eq!(b.components["load"], true);
    assert!(lanes::lane_name("load", "score", 3).starts_with("a-scorer/testnet/load/"));
}

#[test]
fn a_spent_generation_budget_alerts_and_generation_goes_on() {
    let db = Sqlite::open();
    sql::migrate(&db, &EVAL_COHORT_SCHEMA).unwrap();
    let mut p = wide(20, 0);
    p.generation_budget_microusd = 1;
    let seq = open_cohort(&db, &p, 1, 5_000);
    let mut sim = Sim::new(&db, cfg(), 5_010);
    // A budget that cannot hold one item: every item is still generated,
    // and the cohort carries the alert. Nothing pauses for spend.
    assert_eq!(sim.run(seq, 400), "transition");
    assert_eq!(fault_of(&db, seq), None);
    let st = status(&db, sim.now);
    assert_eq!(st["cohorts"][0]["budget_alert"], "generation_budget");
    assert_eq!(st["pool_rows_used"], 8);
    // Abandon still takes only codes, and only for a faulted cohort.
    assert!(tx(&db, |s| engine::abandon(s, &p.label, "Budget Spent", sim.now)).is_err(), "reasons are codes");
}

#[test]
fn requests_that_never_reached_the_model_use_no_attempt_and_repeats_pause() {
    let db = Sqlite::open();
    sql::migrate(&db, &EVAL_COHORT_SCHEMA).unwrap();
    let seq = open_cohort(&db, &wide(20, 0), 10, 1_000_000);
    let mut sim = Sim::new(&db, cfg(), 1_000_010);
    sim.tick_ms = 3_000;
    sim.score = Box::new(|_| Score::NotDispatched);
    sim.run(seq, 400);
    assert_eq!(fault_of(&db, seq).as_deref(), Some("scorer_unavailable"));
    let max_attempt: i64 = db.conn.query_row("SELECT MAX(attempt) FROM chunks", [], |r| r.get(0)).unwrap();
    assert_eq!(max_attempt, 0, "every refused attempt number went back unused");
    // A clear resumes the same cohort: paused lanes are kicked again.
    assert!(tx(&db, |s| engine::clear_fault(s, "testnet-rehearsal-1", sim.now)).unwrap().cleared);
    sim.score = Box::new(|_| Score::Ok);
    assert_eq!(sim.run(seq, 400), "transition");
    for task in 0..20 {
        assert_eq!(numbers(&db, seq, 0, task), [(1, 1)]);
    }
}

#[test]
fn a_generation_outage_pauses_the_cohort_instead_of_dropping_cases() {
    let db = Sqlite::open();
    sql::migrate(&db, &EVAL_COHORT_SCHEMA).unwrap();
    let p = wide(20, 0);
    let seq = open_cohort(&db, &p, 10, 1_000_000);
    let mut sim = Sim::new(&db, cfg(), 1_000_010);
    sim.tick_ms = 10_000;
    sim.generate = Box::new(|_| Err(Reject::Transport));
    sim.run(seq, 400);
    assert_eq!(fault_of(&db, seq).as_deref(), Some("generation_unavailable"));
    let (failed, attempts): (i64, i64) = db
        .conn
        .query_row("SELECT SUM(status = 'failed'), SUM(attempts) FROM docs", [], |r| Ok((r.get(0)?, r.get(1)?)))
        .unwrap();
    assert_eq!((failed, attempts), (0, 0));
    assert!(tx(&db, |s| engine::clear_fault(s, &p.label, sim.now)).unwrap().cleared);
    sim.generate = Box::new(|it| Ok(answer(it)));
    assert_eq!(sim.run(seq, 400), "transition");
}

#[test]
fn an_outage_is_no_answer_while_an_empty_answer_after_tokens_is_the_models() {
    let g = |output, completion_tokens| GenOutcome {
        idx: 0, generator: "x".into(), output, prompt_tokens: 10, completion_tokens, ms: 1, claim: 0,
    };
    assert!(engine::is_outage(&g(Err(Reject::Transport), 0)));
    assert!(engine::is_outage(&g(Err(Reject::Empty), 0)));
    assert!(engine::is_outage(&g(Ok("  ".into()), 0)));
    assert!(!engine::is_outage(&g(Err(Reject::Empty), 1200)), "a reasoning model at its cap answered");
    assert!(!engine::is_outage(&g(Err(Reject::Throttled), 0)), "a throttle is neither an answer nor an outage");
    assert_eq!(textgen::classify_ai_error("AiError: 3040: Capacity temporarily exceeded, please try again."), Reject::Throttled);
    assert_eq!(textgen::classify_ai_error("Error: 429 Too Many Requests"), Reject::Throttled);
    assert_eq!(textgen::classify_ai_error("AiError: 5007: No such model"), Reject::ModelUnavailable);
    assert_eq!(textgen::classify_ai_error("network connection lost"), Reject::Transport);
}

#[test]
fn a_batch_short_of_the_plan_pauses_unless_within_the_declared_tolerance() {
    for permille in [0u16, 50] {
        let db = Sqlite::open();
        sql::migrate(&db, &EVAL_COHORT_SCHEMA).unwrap();
        let mut p = wide(200, 0);
        p.max_kind_shortfall_permille = permille;
        let seq = open_cohort(&db, &p, 10, 1_000_000);
        let mut sim = Sim::new(&db, cfg(), 1_000_010);
        let (pool, texts) = pool_of(120);
        sim.pool = pool;
        sim.texts = texts;
        sim.tick_ms = 2_000;
        // One mixed document is always refused (a length rejection).
        let mut target: Option<i64> = None;
        let dbr = &db;
        sim.generate = Box::new(move |it| {
            let t = *target.get_or_insert_with(|| dbr.conn.query_row("SELECT MIN(idx) FROM docs WHERE kind = 'mixed'", [], |r| r.get(0)).unwrap());
            if it.idx == t { Ok("Approved.".into()) } else { Ok(answer(it)) }
        });
        let end = sim.run(seq, 2_000);
        let st = status(&db, sim.now);
        assert_eq!(st["cohorts"][0]["counts"]["shortfall"], json!([0, 1, 0]));
        if permille == 0 {
            assert_eq!(st["cohorts"][0]["fault"], "batch_shortfall");
        } else {
            assert_eq!(end, "transition", "5 % of 80 mixed cases is 4: the batch seals and binds the shortfall");
            assert_eq!(st["cohorts"][0]["task_count"], 199);
        }
    }
}

#[test]
fn a_draw_never_takes_a_human_the_durable_ledger_records() {
    let db = Sqlite::open();
    sql::migrate(&db, &EVAL_COHORT_SCHEMA).unwrap();
    let (pool, texts) = pool_fixture();
    let now = 1_000_000u64;
    let p = wide(20, 0);
    let seq = open_cohort(&db, &p, 10, now);
    let c = cfg();
    let seed = engine::cohort_seed(&c.private_seed, &p.label);
    let d = pool::draw(&pool, &BTreeSet::new(), &BTreeSet::new(), 8, &seed);
    let t: BTreeMap<String, String> = d.rows.iter().map(|r| (r.row_id.clone(), texts[&r.row_id].clone())).collect();
    let mut ledger = pool::Used::default();
    ledger.digests.insert(d.rows[0].text_sha256.clone());
    let refused = tx(&db, |s| engine::apply_draw(s, &c, seq, &d, &t, &ledger, now));
    assert_eq!(refused.unwrap_err().code(), "pool row or group already used");
    assert_eq!(status(&db, now)["pool_rows_used"], 0);
}

#[test]
fn phase_minimums_count_only_tempos_observed_live() {
    let db = Sqlite::open();
    sql::migrate(&db, &EVAL_COHORT_SCHEMA).unwrap();
    let mut now = 1_000_000u64;
    let p = wide(20, 0); // K = 4, admission 1, transition 1
    let bytes = serde_json::to_vec(&p).unwrap();
    let seq = tx(&db, |s| engine::queue(s, &cfg(), &bytes, now)).unwrap();
    assert!(kick_at(&db, 10, now).is_empty());
    assert_eq!(kick_at(&db, 11, now), ["testnet-rehearsal-1:admission"]);
    now += 1_800_000;
    assert!(kick_at(&db, 16, now).is_empty(), "admission does not end on tempos nobody observed");
    let o = tx(&db, engine::observed).unwrap();
    assert_eq!((o.active_tempos, o.uncredited_tempos), (1, 5));
    now += 60_000;
    assert_eq!(kick_at(&db, 17, now), ["testnet-rehearsal-1:generating"]);
    let mut sim = Sim::new(&db, cfg(), now);
    assert_eq!(sim.run(seq, 400), "transition");
    now = sim.now;
    assert!(kick_at(&db, 18, now).is_empty());
    now += 20 * 60_000;
    assert!(kick_at(&db, 25, now).is_empty());
    now += 60_000;
    assert!(kick_at(&db, 26, now).is_empty());
    assert_eq!(kick_at(&db, 27, now), ["testnet-rehearsal-1:closed"]);
}

#[test]
fn the_prestart_kicks_scoring_lanes_once_during_the_generation_tail() {
    let db = Sqlite::open();
    sql::migrate(&db, &EVAL_COHORT_SCHEMA).unwrap();
    let mut p = wide(40, 1);
    p.fanout.as_mut().unwrap().score.prestart_lead_ms = 120_000;
    p.fanout.as_mut().unwrap().generation.max_lanes = 1;
    p.fanout.as_mut().unwrap().generation.inflight_per_lane = 1;
    let seq = open_cohort(&db, &p, 10, 1_000_000);
    let mut sim = Sim::new(&db, cfg(), 1_000_010);
    sim.tick_ms = 10_000;
    sim.claims_per_run = 1;
    sim.grant = 1;
    let mut kicked_while_generating = false;
    for _ in 0..300 {
        sim.tick();
        let ph = phase(&db, seq);
        let score_kicks: i64 = db.conn.query_row("SELECT SUM(kicks) FROM lanes WHERE seq = 1 AND kind = 'score'", [], |r| r.get(0)).unwrap_or(0);
        if ph == "generating" && score_kicks > 0 && !kicked_while_generating {
            kicked_while_generating = true;
            let pending: i64 = db.conn.query_row("SELECT COUNT(*) FROM docs WHERE status = 'pending'", [], |r| r.get(0)).unwrap();
            assert!(pending > 0, "pre-started while items remained");
            // Pre-started lanes hold no claim: they wait for the seal.
            let claimed: i64 = db.conn.query_row("SELECT COUNT(*) FROM chunks", [], |r| r.get(0)).unwrap();
            assert_eq!(claimed, 0);
        }
        if ph == "transition" {
            break;
        }
    }
    assert!(kicked_while_generating, "pre-started during the generation tail");
    let pre: i64 = db.conn.query_row("SELECT COUNT(*) FROM lane_events WHERE seq = 1 AND kind = 'score' AND event = 'kick'", [], |r| r.get(0)).unwrap();
    let lanes_n: i64 = db.conn.query_row("SELECT COUNT(*) FROM lanes WHERE seq = 1 AND kind = 'score'", [], |r| r.get(0)).unwrap();
    assert_eq!(pre, lanes_n, "each lane was kicked exactly once at the pre-start (no re-kicks needed)");
    verify(&sim, &sim.packages[0]);
}

#[test]
fn the_v3_cohort_store_migrates_in_place_to_v7() {
    let db = Sqlite::open();
    let v3 = sql::Schema { family: EVAL_COHORT_SCHEMA.family, migrations: &EVAL_COHORT_SCHEMA.migrations[..3] };
    sql::migrate(&db, &v3).unwrap();
    let mut v1 = serde_json::to_value(plan(20)).unwrap();
    v1["schema"] = super::plan::PLAN_SCHEMA_V1.into();
    v1["scorer"] = json!({"chunk_docs": 32, "parallel_jobs": 2});
    v1.as_object_mut().unwrap().remove("fanout");
    db.conn
        .execute(
            "INSERT INTO cohorts (label, cohort, plan, plan_sha256, phase, phase_since_ms, queued_ms, opened_epoch,
                closed_epoch, target_digest) VALUES ('testnet-rehearsal-1', 1, ?1, 'ab', 'closed', 1, 1, 9, 13, 'cd')",
            [&v1.to_string()],
        )
        .unwrap();
    db.conn
        .execute(
            "INSERT INTO jobs (seq, job, role, artifact, package_sha256, manifest_key, owner, status, chunks, done_chunks)
             VALUES (1, 0, 'reference', 'aa', 'bb', 'k', '{}', 'done', 2, 2)",
            [],
        )
        .unwrap();
    let report = sql::migrate(&db, &EVAL_COHORT_SCHEMA).unwrap();
    assert_eq!((report.from, report.to), (3, 20));
    let st = status(&db, 5);
    assert_eq!(st["cohorts"][0]["phase"], "closed");
    assert_eq!(st["cohorts"][0]["jobs"][0]["status"], "done", "the jobs table was rebuilt with its rows");
    assert_eq!(st["cohorts"][0]["lanes"], Value::Null, "a v1 cohort has no lanes");
    db.conn.execute("UPDATE jobs SET status = 'faulted' WHERE seq = 1", []).unwrap();
}

#[test]
fn a_predecessor_reference_is_scored_on_the_same_batch_but_never_credited() {
    let db = Sqlite::open();
    sql::migrate(&db, &EVAL_COHORT_SCHEMA).unwrap();
    let mut p = wide(20, 1);
    p.previous_reference = Some(super::plan::tests::artifact(super::plan::tests::identity(0, 0xb0), 0x23));
    let seq = open_cohort(&db, &p, 1, 7_000);
    let mut sim = Sim::new(&db, cfg(), 7_010);
    assert_eq!(sim.run(seq, 400), "transition");
    let b = sim.packages.pop().unwrap();
    let verified = verify(&sim, &b);
    let prev = p.previous_reference.as_ref().unwrap().id().unwrap();
    assert!(verified.iter().any(|r| r.artifact == prev), "R_{{k-1}} has receipts");
    assert_eq!(b.package.scores.len(), 1, "only entrants are credited");
    assert_eq!(b.components["jobs"][1]["role"], "previous_reference");
}

// ------------------------------------------- review of 2026-10-31: regressions

fn lane_events(db: &Sqlite, seq: i64, ev: &str) -> i64 {
    db.conn.query_row("SELECT COUNT(*) FROM lane_events WHERE seq = ?1 AND event = ?2", rusqlite::params![seq, ev], |r| r.get(0)).unwrap()
}

#[test]
fn an_isolation_run_cut_off_by_the_step_budget_continues_under_the_same_attempt() {
    // Finding 1: one isolation run could outlast the 15-minute alarm wall, die
    // uncommitted and spend every attempt. Here each restart costs 120 s of a
    // 600 s step, so a chunk of four always-crashing documents (each proved
    // and reproduced) needs several invocations.
    let db = Sqlite::open();
    sql::migrate(&db, &EVAL_COHORT_SCHEMA).unwrap();
    let seq = open_cohort(&db, &wide(20, 1), 10, 1_000_000);
    let mut sim = Sim::new(&db, cfg(), 1_000_010);
    sim.restart_ms = 120_000;
    sim.score = Box::new(|q| if q.job == 1 && q.tasks.iter().any(|t| *t < 4) { Score::Crash } else { Score::Ok });
    assert_eq!(sim.run(seq, 2_000), "transition", "the cohort proceeds");
    let b = sim.packages.pop().unwrap();
    verify(&sim, &b);
    assert!(lane_events(&db, seq, "iso_yield") >= 2, "the run was handed back and continued");
    for task in 0..4 {
        assert_eq!(numbers(&db, seq, 1, task), [(1, 5), (2, 2)], "one isolation attempt, whatever its invocations");
    }
    assert_eq!(numbers(&db, seq, 1, 4), [(1, 1)]);
    assert_eq!(b.components["jobs"][1]["isolated_crashes"], 4);
    let left: i64 = db.conn.query_row("SELECT COUNT(*) FROM iso_done", [], |r| r.get(0)).unwrap();
    assert_eq!(left, 0, "progress rows end with their commit");
}

#[test]
fn an_entrant_that_crashes_on_everything_is_model_errors_and_the_cohort_proceeds() {
    // Finding 1's scenario: every request of job 1 kills its container, the
    // control included. Its fresh instances cannot score the control, which
    // counts against the artifact until it is recorded as failed.
    let db = Sqlite::open();
    sql::migrate(&db, &EVAL_COHORT_SCHEMA).unwrap();
    let seq = open_cohort(&db, &wide(20, 1), 10, 1_000_000);
    let mut sim = Sim::new(&db, cfg(), 1_000_010);
    sim.score = Box::new(|q| if q.job == 1 { Score::Crash } else { Score::Ok });
    assert_eq!(sim.run(seq, 2_000), "transition");
    let b = sim.packages.pop().unwrap();
    verify(&sim, &b);
    for task in 0..20 {
        let n = numbers(&db, seq, 1, task);
        assert_eq!(n.last().unwrap().1, 2, "task {task}: a model error ({n:?})");
        assert!(n.len() <= 2 && n.iter().take(n.len() - 1).all(|x| x.1 == 5), "task {task}: {n:?}");
        assert_eq!(numbers(&db, seq, 0, task), [(1, 1)]);
    }
    assert!(b.components["jobs"][1]["artifact_failed"].is_string());
    assert!(b.components["jobs"][0]["artifact_failed"].is_null());
    assert_eq!(b.package.scores[0].failed, 20);
}

#[test]
fn an_artifact_that_never_loads_is_model_errors_and_a_reference_that_never_loads_pauses() {
    // Finding 5: load failures were never attributed and re-kicked forever.
    let db = Sqlite::open();
    sql::migrate(&db, &EVAL_COHORT_SCHEMA).unwrap();
    let seq = open_cohort(&db, &wide(20, 1), 10, 1_000_000);
    let mut sim = Sim::new(&db, cfg(), 1_000_010);
    sim.start = Box::new(|job, _| if job == 1 { Err(RestartFailure::Artifact { code: "artifact load failed", definitive: false }) } else { Ok(()) });
    assert_eq!(sim.run(seq, 800), "transition");
    verify(&sim, &sim.packages[0]);
    for task in 0..20 {
        assert_eq!(numbers(&db, seq, 1, task), [(1, 2)]);
    }
    let st = status(&db, sim.now);
    assert_eq!(st["cohorts"][0]["jobs"][1]["artifact_failures"], 3, "the plan's declared count, on fresh instances");
    assert_eq!(st["cohorts"][0]["jobs"][1]["artifact_failed"], "artifact load failed");
    // A manifest over the declared size bound attributes at once.
    let db = Sqlite::open();
    sql::migrate(&db, &EVAL_COHORT_SCHEMA).unwrap();
    let seq = open_cohort(&db, &wide(20, 1), 10, 1_000_000);
    let mut sim = Sim::new(&db, cfg(), 1_000_010);
    sim.start = Box::new(|job, _| {
        if job == 1 { Err(RestartFailure::Artifact { code: "artifact over the declared size bound", definitive: true }) } else { Ok(()) }
    });
    assert_eq!(sim.run(seq, 800), "transition");
    let st = status(&db, sim.now);
    assert_eq!(st["cohorts"][0]["jobs"][1]["artifact_failures"], 1);
    // A reference is never attributed: its failures pause the cohort, resumably.
    let db = Sqlite::open();
    sql::migrate(&db, &EVAL_COHORT_SCHEMA).unwrap();
    let seq = open_cohort(&db, &wide(20, 1), 10, 1_000_000);
    let mut sim = Sim::new(&db, cfg(), 1_000_010);
    let broken = std::rc::Rc::new(std::cell::Cell::new(true));
    let b2 = broken.clone();
    sim.start = Box::new(move |job, _| {
        if job == 0 && b2.get() { Err(RestartFailure::Artifact { code: "artifact load failed", definitive: false }) } else { Ok(()) }
    });
    sim.run(seq, 800);
    assert_eq!(fault_of(&db, seq).as_deref(), Some("scorer_unavailable"));
    broken.set(false);
    assert!(tx(&db, |s| engine::clear_fault(s, "testnet-rehearsal-1", sim.now)).unwrap().cleared);
    assert_eq!(sim.run(seq, 800), "transition");
    for task in 0..20 {
        assert_eq!(numbers(&db, seq, 0, task), [(1, 1)]);
    }
    // A reference refused definitively pauses the cohort at its first
    // refusal, not after `failures_before_pause` cold starts.
    let db = Sqlite::open();
    sql::migrate(&db, &EVAL_COHORT_SCHEMA).unwrap();
    let seq = open_cohort(&db, &wide(20, 1), 10, 1_000_000);
    let mut sim = Sim::new(&db, cfg(), 1_000_010);
    let refused = std::rc::Rc::new(std::cell::Cell::new(0u32));
    let r2 = refused.clone();
    sim.start = Box::new(move |job, _| {
        if job == 0 {
            r2.set(r2.get() + 1);
            Err(RestartFailure::Artifact { code: "refused graph_not_allowed", definitive: true })
        } else {
            Ok(())
        }
    });
    sim.run(seq, 800);
    assert_eq!(fault_of(&db, seq).as_deref(), Some("scorer_unavailable"));
    // A definitive refusal is a cause no probe clears: no probe cold-starts
    // the refused artifact again (L2 review r1, item 8); the operator clears
    // it once a scorer that admits the reference is deployed.
    for _ in 0..200 {
        sim.tick();
    }
    assert!(sim.probes.is_empty(), "no probe of a definitive refusal: {:?}", sim.probes);
    assert_eq!(refused.get(), 1, "one refused load, then paused");
    let st = status(&db, sim.now);
    assert_eq!(st["cohorts"][0]["fault_cause"], "definitive:reference");
    assert_eq!(st["cohorts"][0]["probes"]["probed"], false);
    let st = status(&db, sim.now);
    assert!(st["cohorts"][0]["jobs"][0]["artifact_failed"].is_null(), "a reference is never attributed");
    let note: String = db
        .conn
        .query_row("SELECT note FROM phase_log WHERE seq = ? AND note LIKE '%refused definitively%'", [seq], |r| r.get(0))
        .unwrap();
    assert!(note.contains("graph_not_allowed"), "{note}");
}

#[test]
fn an_r2_read_outage_before_dispatch_pauses_without_spending_attempts_and_the_same_cohort_resumes() {
    // Finding 2: a batch-shard read failure released the claim with its attempt
    // allocated, so three of them faulted the cohort terminally.
    let db = Sqlite::open();
    sql::migrate(&db, &EVAL_COHORT_SCHEMA).unwrap();
    let seq = open_cohort(&db, &wide(20, 1), 10, 1_000_000);
    let mut sim = Sim::new(&db, cfg(), 1_000_010);
    sim.tick_ms = 3_000;
    let outage = std::rc::Rc::new(std::cell::Cell::new(true));
    let o2 = outage.clone();
    sim.r2 = Box::new(move |key, put| o2.get() && !put && key.contains("/batch/"));
    sim.run(seq, 400);
    assert_eq!(fault_of(&db, seq).as_deref(), Some("scorer_unavailable"), "paused, resumable");
    let (max_attempt, receipts): (i64, i64) = db
        .conn
        .query_row("SELECT (SELECT MAX(attempt) FROM chunks), (SELECT COUNT(*) FROM receipts)", [], |r| Ok((r.get(0)?, r.get(1)?)))
        .unwrap();
    assert_eq!((max_attempt, receipts), (0, 0), "no attempt spent, no receipt written");
    outage.set(false);
    assert!(tx(&db, |s| engine::clear_fault(s, "testnet-rehearsal-1", sim.now)).unwrap().cleared);
    assert_eq!(sim.run(seq, 400), "transition");
    for job in 0..2 {
        for task in 0..20 {
            assert_eq!(numbers(&db, seq, job, task), [(1, 1)]);
        }
    }
}

#[test]
fn a_reconciliation_read_error_retries_and_a_durable_results_object_is_never_recomputed() {
    // Finding 2: any R2 GET error in reconciliation counted as absence, so an
    // attempt whose objects were durable could be recorded lost and recomputed.
    for case in ["get_error", "receipt_put_error"] {
        let db = Sqlite::open();
        sql::migrate(&db, &EVAL_COHORT_SCHEMA).unwrap();
        let seq = open_cohort(&db, &wide(20, 1), 10, 1_000_000);
        let mut sim = Sim::new(&db, cfg(), 1_000_010);
        sim.tick_ms = 5_000;
        let mut died = false;
        if case == "get_error" {
            // Both objects are stored, then the lane dies; the first reads fail.
            sim.score = Box::new(move |q| {
                if q.job == 0 && q.chunk == 0 && !std::mem::replace(&mut died, true) { Score::DieAfterPut } else { Score::Ok }
            });
            let mut failures = 3;
            sim.r2 = Box::new(move |key, put| {
                !put && key.contains("/results/job-000/chunk-0000-attempt-1") && {
                    failures -= 1;
                    failures >= 0
                }
            });
        } else {
            // The results object is stored; every receipt put of that attempt fails.
            let mut failures = 7;
            sim.r2 = Box::new(move |key, put| {
                put && key.ends_with("/job-000/chunk-0000-attempt-1.bin") && {
                    failures -= 1;
                    failures >= 0
                }
            });
        }
        assert_eq!(sim.run(seq, 600), "transition", "{case}");
        verify(&sim, &sim.packages[0]);
        for task in 0..4 {
            assert_eq!(numbers(&db, seq, 0, task), [(1, 1)], "{case}: committed from the durable results, no compute");
        }
        let st = status(&db, sim.now);
        let score = &st["cohorts"][0]["lanes"]["kinds"]["score"];
        assert_eq!((score["reconciled"].as_i64(), score["lost_attempts"].as_i64()), (Some(1), Some(0)), "{case}");
    }
}

#[test]
fn a_failure_with_the_container_up_never_isolates_and_a_one_off_isolated_failure_is_not_attributed() {
    // Finding 4: any failure counted as a crash, and one failure in isolation
    // was attributed without being reproduced.
    let db = Sqlite::open();
    sql::migrate(&db, &EVAL_COHORT_SCHEMA).unwrap();
    let seq = open_cohort(&db, &wide(20, 1), 10, 1_000_000);
    let mut sim = Sim::new(&db, cfg(), 1_000_010);
    let (mut fail_once, mut crash_once, mut flaky_once) = (true, true, true);
    sim.score = Box::new(move |q| {
        if q.job == 0 && q.chunk == 0 && std::mem::take(&mut fail_once) {
            return Score::Fail;
        }
        if q.job == 1 && q.chunk == 1 && !q.isolate && std::mem::take(&mut crash_once) {
            return Score::Crash;
        }
        if q.job == 1 && q.isolate && q.tasks == [5] && std::mem::take(&mut flaky_once) {
            return Score::Crash;
        }
        Score::Ok
    });
    assert_eq!(sim.run(seq, 800), "transition");
    verify(&sim, &sim.packages[0]);
    let crashes: i64 = db.conn.query_row("SELECT crashes FROM chunks WHERE seq = ?1 AND job = 0 AND chunk = 0", [seq], |r| r.get(0)).unwrap();
    assert_eq!(crashes, 0, "a failure with the container up is not a crash");
    assert_eq!(numbers(&db, seq, 0, 0), [(1, 5), (2, 1)]);
    // Task 5 failed alone once, then answered on the proven fresh instance.
    assert_eq!(numbers(&db, seq, 1, 5), [(1, 5), (2, 1)]);
    assert_eq!(sim.packages[0].components["jobs"][1]["isolated_crashes"], 0);
}

#[test]
fn an_isolation_restart_that_fails_hands_back_without_spending_an_attempt() {
    // Finding 2: an isolation run whose first restart failed committed every
    // document as an infrastructure error, a full attempt with nothing dispatched.
    let db = Sqlite::open();
    sql::migrate(&db, &EVAL_COHORT_SCHEMA).unwrap();
    let seq = open_cohort(&db, &wide(20, 0), 10, 1_000_000);
    let mut sim = Sim::new(&db, cfg(), 1_000_010);
    let mut crash_once = true;
    sim.score = Box::new(move |q| if q.chunk == 0 && !q.isolate && std::mem::take(&mut crash_once) { Score::Crash } else { Score::Ok });
    let mut fails = 3;
    sim.start = Box::new(move |_, isolation| {
        if isolation && fails > 0 {
            fails -= 1;
            return Err(RestartFailure::Infra("container did not answer"));
        }
        Ok(())
    });
    assert_eq!(sim.run(seq, 800), "transition");
    for task in 0..4 {
        assert_eq!(numbers(&db, seq, 0, task), [(1, 5), (2, 1)]);
    }
}

// ----------------------------------------------------- reward-v1 (plan v3)

fn v3_block(probes: Vec<super::plan::ProbeRef>) -> super::plan::RewardV1Plan {
    super::plan::RewardV1Plan {
        sigma: 40_000_000_000_000_000,
        sigma_basis: "synthetic test value".into(),
        lease_tempos: 8,
        reuse: super::plan::Reuse { max_uses: 4, min_gap: 7 },
        probes,
        gates: super::plan::GatePolicy {
            operating_ppb: 500_000_000,
            loose_fpr_max_permille: 100,
            strict_fpr_max_permille: 50,
            share_mae_max_ppb: 1_000_000_000,
            attribution_min_permille: 0,
        },
        canary: super::plan::CanaryPolicy { draw_humans: 4, drill: Vec::new() },
        spares_permille: 200,
        output_contract: "slop_ninja.a.document-v2".into(),
        probe_version: None,
        incentive: None,
    }
}

fn plan_v3(cohort: u64, label: &str, n: u32, reference: super::plan::ArtifactJson) -> CohortPlan {
    let mut p = plan(n);
    p.schema = super::plan::PLAN_SCHEMA_V3.into();
    p.cohort = cohort;
    p.label = label.into();
    p.reward_rule = "reward-v1".into();
    p.mix = super::plan::Mix { human_only: 20, mixed: 55, model_only: 25 };
    p.k_tempos = 4;
    p.admission_tempos = 1;
    p.transition_tempos = 2;
    p.scorer.chunk_docs = 4;
    p.fanout.as_mut().unwrap().score.lane_docs = 8;
    p.reference = reference;
    p.entrants = Vec::new();
    p.reward_v1 = Some(v3_block(Vec::new()));
    p
}

/// A synthetic scorer artifact: its files manifest goes to the sim's eval
/// bucket; `files` names each inference file's distinguishing tag.
fn v3_artifact(sim: &mut Sim<'_>, owner: super::plan::IdentityJson, files: [u8; 5]) -> super::plan::ArtifactJson {
    let roles = ["encoder_graph", "heads", "tokenizer", "calibration", "contract"];
    let file_list: Vec<Value> = roles
        .iter()
        .zip(files)
        .map(|(role, tag)| json!({"name": format!("{role}.bin"), "role": role, "bytes": 100 + u64::from(tag),
            "sha256": hex::encode([tag; 32]), "parts": []}))
        .chain(std::iter::once(json!({"name": "manifest.json", "role": "export_manifest", "bytes": 7,
            "sha256": hex::encode([owner.uid as u8; 32]), "parts": []})))
        .collect();
    let m = json!({"schema": "slop_ninja_a_artifact_files_v1", "profile_id": "synthetic", "contract": "slop_ninja_full_a_contract_v2",
        "graph": "encoder_graph.bin", "files": file_list});
    let bytes = serde_json::to_vec(&m).unwrap();
    let sha = lane_logic::sha_hex(&bytes);
    let key = format!("artifacts/packages/{sha}.json");
    sim.eval.insert(key.clone(), bytes);
    let mut a = super::plan::tests::artifact(owner, 0);
    a.package_sha256 = sha;
    a.manifest_key = key;
    a
}

/// Deterministic pseudo-noise in [-1, 1] for (model, task).
fn noise(model: u8, task: u32) -> f64 {
    let d = Sha256::digest([&[model][..], &task.to_le_bytes()].concat());
    (f64::from(u16::from_le_bytes([d[0], d[1]])) / 32767.5) - 1.0
}

/// Confidence % from per-kind bases and a model's noise.
fn model_conf(model: u8, kind: &str, task: u32, bases: (f64, f64, f64), amp: f64) -> f64 {
    let base = match kind {
        "human_only" | "control" => bases.0,
        "mixed" => bases.1,
        _ => bases.2,
    };
    (base + amp * noise(model, task)).clamp(1.0, 99.0)
}

fn run_to_close(sim: &mut Sim<'_>, db: &Sqlite, seq: i64, first_epoch: u64) -> u64 {
    assert_eq!(sim.run(seq, 3_000), "transition", "fault {:?}", fault_of(db, seq));
    let mut e = first_epoch;
    for _ in 0..12 {
        e += 1;
        kick_at(db, e, sim.now);
        sim.tick();
        if phase(db, seq) == "closed" {
            sim.tick();
            return e;
        }
    }
    panic!("cohort {seq} did not close: {}", phase(db, seq));
}

#[test]
fn reward_v1_cohorts_run_end_to_end_with_probe_copies_canary_and_promotion() {
    let db = Sqlite::open();
    sql::migrate(&db, &EVAL_COHORT_SCHEMA).unwrap();
    let mut sim = Sim::new(&db, cfg(), 2_000_000);
    let (pool, texts) = pool_of(120);
    sim.pool = pool;
    sim.texts = texts;
    let id = super::plan::tests::identity;
    // The rehearsal reference (a flattened calibration), the released model,
    // a distinct variant, an exact copy of the reference and a near copy.
    let weak = v3_artifact(&mut sim, id(0, 0xb0), [1, 2, 3, 4, 5]);
    let strong = v3_artifact(&mut sim, id(11, 0xa1), [1, 2, 3, 9, 5]);
    let variant = v3_artifact(&mut sim, id(12, 0xa2), [1, 7, 3, 9, 5]);
    let copy = v3_artifact(&mut sim, id(13, 0xa3), [1, 2, 3, 4, 5]);
    let near = v3_artifact(&mut sim, id(14, 0xa4), [1, 2, 3, 6, 5]);
    let weak_conf = |kind: &str, task: u32| model_conf(1, kind, task, (35.0, 65.0, 70.0), 10.0);
    // Probe build v1: R_0 is fingerprinted.
    let mut probe = plan_v3(1, "testnet-probe-v1", 20, weak.clone());
    probe.schema = super::plan::PROBE_PLAN_SCHEMA.into();
    probe.reward_v1.as_mut().unwrap().probe_version = Some(1);
    probe.reward_v1.as_mut().unwrap().canary.draw_humans = 0;
    sim.predict = Some(Box::new(move |_, kind, task| {
        let c = weak_conf(kind, task);
        (c, c * 0.4)
    }));
    let pseq = open_cohort(&db, &probe, 10, sim.now);
    let end = sim.run(pseq, 3_000);
    assert_eq!(end, "closed", "fault {:?}: {}", fault_of(&db, pseq), {
        let st = status(&db, sim.now);
        serde_json::to_string(&json!({"jobs": st["cohorts"][0]["jobs"], "log": st["cohorts"][0]["log"], "lanes": st["cohorts"][0]["lanes"]})).unwrap()
    });
    let record = sim.eval["probes/v1/record.json"].clone();
    let probe_ref = super::plan::ProbeRef {
        version: 1,
        size: 20,
        key: "probes/v1/record.json".into(),
        sha256: lane_logic::sha_hex(&record),
    };
    let (index, _) = tx(&db, v3::published_index).unwrap();
    assert_eq!((index.contents.contents.len(), index.fingerprints.entries.len()), (1, 1));
    // Cohort A: R_0 alone produces no target (the launch burn row stays).
    let mut a = plan_v3(2, "testnet-rv1-a", 20, weak.clone());
    a.reward_v1.as_mut().unwrap().probes = vec![probe_ref.clone()];
    let seq_a = open_cohort(&db, &a, 20, sim.now);
    run_to_close(&mut sim, &db, seq_a, 22);
    let pa = sim.packages_v3.pop().unwrap();
    assert_eq!(pa.package.mode, slop_ninja_protocol::CutoffMode::NoTarget);
    assert!(pa.target.is_none());
    let ca = sim.closures.pop().unwrap();
    ca.closure.check(&pa.manifest, &pa.package, None).unwrap();
    // Cohort B: the released model and a variant beat R_0 by the margin; an
    // exact copy is never executed; a near copy fails the public-copy gate;
    // the canary flags the top head twice, so the next head is promoted.
    let mut b = plan_v3(3, "testnet-rv1-b", 20, weak.clone());
    b.entrants = vec![strong.clone(), variant.clone(), copy.clone(), near.clone()];
    b.allow_list = vec![id(11, 0xa1), id(12, 0xa2), id(13, 0xa3), id(14, 0xa4)];
    {
        let v = b.reward_v1.as_mut().unwrap();
        v.probes = vec![probe_ref.clone()];
        v.canary.drill = vec![super::plan::CanaryDrill { hotkey: format!("0x{}", hex::encode([0xa1; 32])), flags: 2 }];
    }
    sim.predict = Some(Box::new(move |job, kind, task| {
        let c = match job {
            0 => weak_conf(kind, task),
            1 => model_conf(2, kind, task, (3.0, 95.0, 97.0), 7.0),
            2 => model_conf(3, kind, task, (7.0, 90.0, 94.0), 7.0),
            _ => (weak_conf(kind, task) + 1.0).min(99.0),
        };
        (c, c * 0.4)
    }));
    let seq_b = open_cohort(&db, &b, 40, sim.now);
    run_to_close(&mut sim, &db, seq_b, 42);
    let pb = sim.packages_v3.pop().unwrap();
    let p = &pb.package;
    assert_eq!(p.mode, slop_ninja_protocol::CutoffMode::Margin);
    let hk = |t: u8| AccountId([t; 32]);
    assert_eq!(p.entrants[2].public_copy(), Some(slop_ninja_protocol::PublicCopy::Content));
    assert_eq!(p.entrants[2].score, None, "an exact copy of a published artifact is never executed");
    assert_eq!(p.entrants[3].public_copy(), Some(slop_ninja_protocol::PublicCopy::Current));
    assert_eq!(p.promotion_audits.len(), 2);
    assert!(p.promotion_audits[0].blocked() && p.promotion_audits[0].head == hk(0xa1));
    assert_eq!(p.promoted, Some(hk(0xa2)), "two flags block the top head; the next head is promoted");
    assert!(p.entrants[0].units > 0 && p.entrants[1].units > 0, "credits are paid as usual");
    let at_seal = v3::index_at_seal(&tx(&db, |s| v3::state(s, seq_b)).unwrap()).unwrap();
    slop_ninja_protocol::reward_v1::check_outcome(p, &at_seal).unwrap();
    let target = pb.target.clone().unwrap();
    target.check_package(p).unwrap();
    let cb = sim.closures.pop().unwrap();
    let checked = cb.closure.check(&pb.manifest, p, Some(&target)).unwrap();
    // The index after the closure is the one the next cohort binds.
    let after = at_seal.after_closure(p, cb.closure_fingerprints.clone()).unwrap();
    let (now_index, _) = tx(&db, v3::published_index).unwrap();
    assert_eq!(after, now_index);
    // Synthetic owners are unregistered: every unit burns through the owner's UID.
    let snapshot = slop_ninja_protocol::RegistrationSnapshotV2 {
        chain: cfg().chain,
        block: 9_000,
        block_hash: slop_ninja_protocol::Digest([9; 32]),
        owner_hotkey: hk(0xb0),
        neurons: vec![NeuronIdentity { uid: 0, hotkey: hk(0xb0), registration_block: 1000 }],
    };
    let row = slop_ninja_protocol::reward_v1::derive_row(&target, Some((p, &at_seal)), &snapshot).unwrap();
    assert_eq!(row.entries.len(), 1);
    assert_eq!((row.entries[0].uid, row.entries[0].weight), (0, u16::MAX));
    let _ = checked;
    // The reward-v1 bundle a validator installs: every record signed, the
    // receipts retained, the authorization issued at the closing boundary.
    let fx_keys = slop_ninja_protocol::fixtures::trusted_keys();
    let pk = slop_ninja_protocol::fixtures::package_key();
    let signed = |b: Vec<u8>| hex::encode(b);
    let (rindex, chunk_map) = tx(&db, |s| engine::receipt_index(s, seq_b)).unwrap();
    let chunks: Vec<String> = chunk_map["objects"]
        .as_array()
        .unwrap()
        .iter()
        .map(|o| hex::encode(&sim.rcpt[o["key"].as_str().unwrap()]))
        .collect();
    let auth = slop_ninja_protocol::RewardIntentAuthorization {
        chain: p.chain.clone(),
        cohort: p.cohort,
        cohort_manifest: p.cohort_manifest,
        score_package: p.digest().unwrap(),
        target: target.digest().unwrap(),
        checkpoint_generation: 1,
        issued_epoch: cb.closure.closed_epoch,
        renew_by_epoch: cb.closure.closed_epoch + u64::from(pb.manifest.schedule.lease_tempos),
    };
    let bundle = crate::target::BundleV2 {
        schema: crate::target::BUNDLE_SCHEMA_V2.into(),
        shortcut: "T12".into(),
        source: Some(crate::target::SourceHex {
            manifest: signed(slop_ninja_protocol::sign(pb.manifest.clone(), &pk).unwrap().to_bytes().unwrap()),
            package: signed(slop_ninja_protocol::sign(p.clone(), &pk).unwrap().to_bytes().unwrap()),
            target: signed(slop_ninja_protocol::sign(target.clone(), &pk).unwrap().to_bytes().unwrap()),
            closure: signed(slop_ninja_protocol::sign(cb.closure.clone(), &pk).unwrap().to_bytes().unwrap()),
            receipt_index: Some(signed(
                slop_ninja_protocol::sign(rindex, &slop_ninja_protocol::fixtures::receipt_key()).unwrap().to_bytes().unwrap(),
            )),
            published_contents: hex::encode(at_seal.contents.to_bytes().unwrap()),
            fingerprint_index: hex::encode(at_seal.fingerprints.to_bytes().unwrap()),
        }),
        authorization: crate::target::AuthorizationHex {
            kind: "cohort".into(),
            authorization: signed(
                slop_ninja_protocol::sign(auth, &slop_ninja_protocol::fixtures::authorization_key()).unwrap().to_bytes().unwrap(),
            ),
            renewal: None,
            carrier: None,
        },
        receipt_chunks: chunks,
        carrier_receipt_chunks: Vec::new(),
    };
    let bytes = serde_json::to_vec(&bundle).unwrap();
    // A holder that held the launch burn row: cohort B's target is its successor.
    let held = slop_ninja_protocol::ActiveTarget::launch(&p.chain).unwrap();
    let v = crate::target::verify_v2(&crate::target::parse_v2(&bytes).unwrap(), &fx_keys, &TESTNET_GENESIS.0, 601, &held).unwrap();
    assert_eq!(v.derive_row(&snapshot).unwrap(), row, "the validator's row is the coordinator's, bit for bit");
    let vhk = AccountId([0x66; 32]);
    let closed = cb.closure.closed_epoch;
    assert!(v.operation(&fx_keys, &[], vhk, closed).is_ok());
    assert!(v.operation(&fx_keys, &[], vhk, closed - 1).is_err(), "never before the target activates");
    // A tampered unit split is refused before any row exists.
    let mut tampered = p.clone();
    tampered.entrants[0].units -= 1;
    tampered.entrants[1].units += 1;
    let mut bad = bundle.clone();
    bad.source.as_mut().unwrap().package = signed(slop_ninja_protocol::sign(tampered, &pk).unwrap().to_bytes().unwrap());
    let bad = serde_json::to_vec(&bad).unwrap();
    assert!(crate::target::verify_v2(&crate::target::parse_v2(&bad).unwrap(), &fx_keys, &TESTNET_GENESIS.0, 601, &held).is_err());
    // Once the target is held, the launch bundle is refused (no rollback).
    let launch_auth = slop_ninja_protocol::LaunchAuthorization::new(&p.chain, 1, closed).unwrap();
    let launch = crate::target::BundleV2 {
        schema: crate::target::BUNDLE_SCHEMA_V2.into(),
        shortcut: "T12".into(),
        source: None,
        authorization: crate::target::AuthorizationHex {
            kind: "launch".into(),
            authorization: signed(
                slop_ninja_protocol::sign(launch_auth, &slop_ninja_protocol::fixtures::authorization_key()).unwrap().to_bytes().unwrap(),
            ),
            renewal: None,
            carrier: None,
        },
        receipt_chunks: Vec::new(),
        carrier_receipt_chunks: Vec::new(),
    };
    let launch = serde_json::to_vec(&launch).unwrap();
    assert!(crate::target::verify_v2(&crate::target::parse_v2(&launch).unwrap(), &fx_keys, &TESTNET_GENESIS.0, 601, &held).is_ok());
    assert_eq!(
        crate::target::verify_v2(&crate::target::parse_v2(&launch).unwrap(), &fx_keys, &TESTNET_GENESIS.0, 601, &v.active)
            .unwrap_err()
            .code,
        "bundle_unverified"
    );
    // Cohort C: R_1 is the promoted variant with R_0 as its predecessor.
    let mut c = plan_v3(4, "testnet-rv1-c", 20, variant.clone());
    c.previous_reference = Some(weak.clone());
    let copy_of_strong = v3_artifact(&mut sim, id(15, 0xa5), [1, 2, 3, 9, 5]);
    c.entrants = vec![copy_of_strong];
    c.allow_list = vec![id(15, 0xa5)];
    c.reward_v1.as_mut().unwrap().probes = vec![probe_ref];
    sim.predict = Some(Box::new(move |job, kind, task| {
        let c = match job {
            0 => model_conf(3, kind, task, (7.0, 90.0, 94.0), 7.0),
            1 => weak_conf(kind, task),
            _ => model_conf(2, kind, task, (3.0, 95.0, 97.0), 7.0),
        };
        (c, c * 0.4)
    }));
    let seq_c = open_cohort(&db, &c, 60, sim.now);
    run_to_close(&mut sim, &db, seq_c, 62);
    let pc = sim.packages_v3.pop().unwrap();
    assert_eq!(pc.package.reference_index, 1);
    assert_eq!(pc.package.entrants[0].public_copy(), Some(slop_ninja_protocol::PublicCopy::Content));
    let at_seal_c = v3::index_at_seal(&tx(&db, |s| v3::state(s, seq_c)).unwrap()).unwrap();
    slop_ninja_protocol::reward_v1::check_outcome(&pc.package, &at_seal_c).unwrap();
    let st = status(&db, sim.now);
    assert!(st["cohorts"].as_array().unwrap().iter().all(|c| c["fault"].is_null()));
}

/// A world with probe version 1 built over a flattened-calibration `R_0`.
fn probe_built<'a>(db: &'a Sqlite) -> (Sim<'a>, super::plan::ArtifactJson, super::plan::ProbeRef) {
    sql::migrate(db, &EVAL_COHORT_SCHEMA).unwrap();
    let mut sim = Sim::new(db, cfg(), 2_000_000);
    let (pool, texts) = pool_of(120);
    sim.pool = pool;
    sim.texts = texts;
    let weak = v3_artifact(&mut sim, super::plan::tests::identity(0, 0xb0), [1, 2, 3, 4, 5]);
    let mut probe = plan_v3(1, "testnet-probe-v1", 20, weak.clone());
    probe.schema = super::plan::PROBE_PLAN_SCHEMA.into();
    probe.reward_v1.as_mut().unwrap().probe_version = Some(1);
    probe.reward_v1.as_mut().unwrap().canary.draw_humans = 0;
    sim.predict = Some(Box::new(|_, kind, task| {
        let c = model_conf(1, kind, task, (35.0, 65.0, 70.0), 10.0);
        (c, c * 0.4)
    }));
    let pseq = open_cohort(db, &probe, 10, sim.now);
    assert_eq!(sim.run(pseq, 3_000), "closed");
    let record = sim.eval["probes/v1/record.json"].clone();
    let probe_ref = super::plan::ProbeRef {
        version: 1,
        size: 20,
        key: "probes/v1/record.json".into(),
        sha256: lane_logic::sha_hex(&record),
    };
    (sim, weak, probe_ref)
}

#[test]
fn v3_failing_artifacts_score_zero_and_one_canary_flag_with_a_clean_reaudit_promotes() {
    let db = Sqlite::open();
    let (mut sim, weak, probe_ref) = probe_built(&db);
    let id = super::plan::tests::identity;
    let crashes = v3_artifact(&mut sim, id(21, 0xc1), [1, 2, 3, 21, 5]);
    let never = v3_artifact(&mut sim, id(22, 0xc2), [1, 2, 3, 22, 5]);
    let good = v3_artifact(&mut sim, id(23, 0xc3), [1, 2, 3, 23, 5]);
    let mut p = plan_v3(2, "testnet-rv1-fail", 20, weak);
    p.entrants = vec![crashes, never, good];
    p.allow_list = vec![id(21, 0xc1), id(22, 0xc2), id(23, 0xc3)];
    {
        let v = p.reward_v1.as_mut().unwrap();
        v.probes = vec![probe_ref];
        v.canary.drill = vec![super::plan::CanaryDrill { hotkey: format!("0x{}", hex::encode([0xc3; 32])), flags: 1 }];
    }
    sim.predict = Some(Box::new(|job, kind, task| {
        let c = match job {
            0 => model_conf(1, kind, task, (35.0, 65.0, 70.0), 10.0),
            _ => model_conf(4, kind, task, (3.0, 95.0, 97.0), 7.0),
        };
        (c, c * 0.4)
    }));
    // Job 1 kills its container on every request; job 2 never loads.
    sim.score = Box::new(|q| if q.job == 1 { Score::Crash } else { Score::Ok });
    sim.start = Box::new(|job, _| if job == 2 { Err(RestartFailure::Artifact { code: "artifact load failed", definitive: false }) } else { Ok(()) });
    let seq = open_cohort(&db, &p, 20, sim.now);
    run_to_close(&mut sim, &db, seq, 22);
    let b = sim.packages_v3.pop().unwrap();
    let e = &b.package.entrants;
    // Attributed to those artifacts only: every document a model error, score 0.
    for k in [0, 1] {
        assert_eq!((e[k].score.map(|s| s.get()), e[k].failed, e[k].succeeded), (Some(0), 20, 0), "entrant {k}");
        assert!(e[k].gate_failures.iter().any(|g| g.gate == slop_ninja_protocol::Gate::HumanFalsePositive));
    }
    // One flag triggers a re-audit on a fresh draw; a clean re-audit promotes.
    assert_eq!(b.package.mode, slop_ninja_protocol::CutoffMode::Margin);
    let audit = &b.package.promotion_audits[0];
    assert_eq!(audit.canary.len(), 2);
    assert!(audit.canary[0].flag.as_deref() == Some(v3::DRILL_REASON) && audit.canary[1].flag.is_none());
    assert_ne!(audit.canary[0].draw, audit.canary[1].draw);
    assert_eq!(b.package.promoted, Some(AccountId([0xc3; 32])));
    assert_eq!(e[2].units, u16::MAX);
}

#[test]
fn v3_plans_validate_their_reward_v1_parameters() {
    let none = Ceilings { roster_cap: Some(40), accept_reward_v0: false };
    let weak = super::plan::tests::artifact(super::plan::tests::identity(0, 0xb0), 0x22);
    let probe = super::plan::ProbeRef { version: 1, size: 1024, key: "probes/v1/record.json".into(), sha256: hex::encode([1; 32]) };
    let mut ok = plan_v3(2, "testnet-rv1-ok", 1000, weak.clone());
    ok.reward_v1.as_mut().unwrap().probes = vec![probe.clone()];
    // The strict tier's audit draw holds at least the cohort's 200 humans.
    ok.reward_v1.as_mut().unwrap().canary.draw_humans = 199;
    assert!(refused_plan(&ok, none).starts_with("reward_v1.canary.draw_humans"));
    ok.reward_v1.as_mut().unwrap().canary.draw_humans = 200;
    ok.validate(none).unwrap();
    let refused = |p: CohortPlan| p.validate(none).unwrap_err().0;
    // The mix is exact at n: 55% of 10 is not whole.
    let mut p = ok.clone();
    p.n = 10;
    assert!(refused(p).starts_with("mix"));
    let mut p = ok.clone();
    p.mix = super::plan::Mix { human_only: 20, mixed: 55, model_only: 30 };
    assert!(refused(p).starts_with("mix"));
    let mut p = ok.clone();
    p.reward_v1.as_mut().unwrap().lease_tempos = 7;
    assert!(refused(p).starts_with("reward_v1.lease_tempos"));
    let mut p = ok.clone();
    p.reward_v1.as_mut().unwrap().probes.clear();
    assert!(refused(p).starts_with("reward_v1.probes"));
    let mut p = ok.clone();
    p.reward_v1.as_mut().unwrap().probes[0].size = 0;
    assert!(refused(p).starts_with("reward_v1.probes"));
    let mut p = ok.clone();
    p.reward_v1.as_mut().unwrap().canary.drill = vec![super::plan::CanaryDrill { hotkey: "0x00".into(), flags: 1 }];
    assert!(refused(p).starts_with("reward_v1.canary.drill"));
    let mut p = ok.clone();
    p.max_kind_shortfall_permille = 1;
    assert!(refused(p).starts_with("max_kind_shortfall_permille"));
    let mut p = ok.clone();
    p.transition_tempos = 1;
    assert!(refused(p).starts_with("transition_tempos"));
    let mut p = ok.clone();
    p.reward_rule = "testnet-reward-v0".into();
    assert_eq!(refused(p), "reward_rule");
    // The deployed main instance refuses new reward-v0 plans.
    assert_eq!(refused(plan(200)), "schema (v3 or probe build)");
    plan(200).validate(Ceilings { roster_cap: Some(40), accept_reward_v0: true }).unwrap();
    // The roster cap is the deployment's.
    let mut p = ok.clone();
    p.entrants = (1..=41u16).map(|u| super::plan::tests::artifact(super::plan::tests::identity(u, u as u8), 0x40 + u as u8)).collect();
    p.allow_list = (1..=41u16).map(|u| super::plan::tests::identity(u, u as u8)).collect();
    assert!(refused(p.clone()).starts_with("entrants"));
    p.entrants.pop();
    p.allow_list.pop();
    p.validate(none).unwrap();
    // A probe build names its version and no probes in use.
    let mut b = ok.clone();
    b.schema = super::plan::PROBE_PLAN_SCHEMA.into();
    b.entrants.clear();
    b.allow_list.clear();
    assert!(refused(b.clone()).starts_with("reward_v1.probe_version"));
    b.reward_v1.as_mut().unwrap().probe_version = Some(1);
    assert!(refused(b.clone()).starts_with("reward_v1.probes"));
    b.reward_v1.as_mut().unwrap().probes.clear();
    b.validate(none).unwrap();
}

#[test]
fn v3_rewrite_sources_follow_the_k_and_g_rule() {
    let db = Sqlite::open();
    sql::migrate(&db, &EVAL_COHORT_SCHEMA).unwrap();
    // Ordinals count drawn reward-v1 cohorts, not operator cohort numbers.
    let insert = |row: &str, first: i64, uses: i64, last: Option<i64>, pool: &str| {
        db.conn
            .execute(
                "INSERT INTO reuse_sources (row_id, text_sha256, source_group, domain, shard, first_cohort, uses, last_cohort,
                    first_ordinal, last_ordinal, pool_id)
                 VALUES (?1, ?2, ?3, 'wikipedia', 0, ?4, ?5, ?6, ?4, ?6, ?7)",
                rusqlite::params![row, format!("sha-{row}"), format!("g-{row}"), first, uses, last, pool],
            )
            .unwrap();
    };
    insert("never", 3, 0, None, "p"); // a negative never parented: eligible at once
    insert("recent", 5, 1, Some(5), "p"); // parented at ordinal 5: eligible from 12
    insert("old", 2, 1, Some(2), "p"); // parented at 2: eligible from 9
    insert("spent", 1, 4, Some(4), "p"); // four uses: retired
    insert("own", 10, 1, Some(10), "p"); // drawn in ordinal 10 itself: not earlier
    insert("other-pool", 1, 0, None, "q"); // another pool's passage is never a candidate
    let weak = super::plan::tests::artifact(super::plan::tests::identity(0, 0xb0), 0x22);
    let ids = |ordinal: u64| {
        let p = plan_v3(99, "x", 20, weak.clone());
        tx(&db, |s| v3::reuse_candidates_at(s, &p, ordinal)).unwrap().into_iter().map(|c| c.row.row_id).collect::<Vec<_>>()
    };
    assert_eq!(ids(10), ["old", "never"], "oldest last use first; G = 7 from the last use");
    assert_eq!(ids(12), ["old", "never", "recent"]);
    assert_eq!(ids(3), Vec::<String>::new());
    // The cohort's ordinal ignores probe builds and the operator's numbering.
    let p = plan_v3(99, "x", 20, weak.clone());
    assert_eq!(tx(&db, |s| v3::reuse_ordinal(s, p.cohort)).unwrap(), 1);
    let (all, ineligible) = {
        let mut p = plan_v3(99, "x", 20, weak.clone());
        p.cohort = 1;
        tx(&db, |s| v3::ledger_rows(s, &p)).unwrap()
    };
    assert_eq!(all.len(), 5);
    assert!(ineligible.contains("spent") && ineligible.contains("own") && !ineligible.contains("never"));
    // Parents: same-cohort negatives first, then earlier passages, then fresh
    // groups taken whole.
    let row = |id: &str, g: &str| PoolRow { row_id: id.into(), text_sha256: String::new(), domain: "wikipedia".into(), source_group: g.into(), shard: 0 };
    let texts: BTreeMap<String, String> =
        ["n1", "e1", "f1", "f2", "f3"].iter().map(|k| ((*k).to_owned(), human_text(1, 0))).collect();
    let earlier = vec![v3::ReuseCandidate {
        row: row("e1", "ge"),
        uses: 1,
        generators: vec!["@cf/x".into()],
        voices: vec!["copy_editor".into()],
        operations: vec!["paraphrase".into()],
    }];
    let fresh = vec![row("f1", "g1"), row("f2", "g1"), row("f3", "g2")];
    // A same-cohort negative past the reuse limit parents nothing.
    let skip: BTreeSet<String> = ["n1".to_owned()].into();
    let ps = v3::choose_parents(&[7; 32], &[row("n1", "gn")], &earlier, &fresh, &texts, 3, &skip);
    assert_eq!(ps.children, vec![("e1".into(), "earlier"), ("f1".into(), "fresh"), ("f2".into(), "fresh")]);
    let ps = v3::choose_parents(&[7; 32], &[row("n1", "gn")], &earlier, &fresh, &texts, 3, &BTreeSet::new());
    assert_eq!(ps.children, vec![("n1".into(), "same"), ("e1".into(), "earlier"), ("f1".into(), "fresh")]);
    assert_eq!(ps.fresh_used.iter().map(|r| r.row_id.as_str()).collect::<Vec<_>>(), ["f1", "f2"], "the whole group is consumed");
}

#[test]
fn v3_gates_measure_share_attribution_and_the_loose_false_positive_tier() {
    let labels: Vec<v3::Label> = ["human_only", "human_only", "human_only", "human_only", "mixed", "model_only"]
        .iter()
        .map(|k| v3::Label { set: "batch".into(), kind: (*k).into(), share_ppb: if *k == "mixed" { 400_000_000 } else { 0 } })
        .collect();
    let p = |v: u32| Some(slop_ninja_protocol::Probability::new(v).unwrap());
    let mut plan = v3_block(Vec::new());
    plan.gates.share_mae_max_ppb = 100_000_000;
    plan.gates.attribution_min_permille = 500;
    plan.gates.loose_fpr_max_permille = 250;
    let ok = v3::JobVectors {
        involvement: vec![p(100_000_000), p(200_000_000), p(100_000_000), p(600_000_000), p(900_000_000), p(990_000_000)],
        share: vec![Some(0), Some(0), Some(0), Some(0), Some(450_000_000), Some(990_000_000)],
        roles: vec![true; 6],
    };
    assert!(v3::gates_for_test(&plan, &ok, &labels, 6).is_empty(), "one human in four at the operating point");
    // Two of four humans flagged (one at the point, one failed) fails the loose tier.
    let mut fpr = ok.clone();
    fpr.involvement[1] = None;
    let g = v3::gates_for_test(&plan, &fpr, &labels, 6);
    assert_eq!(g.iter().map(|x| x.reason.as_str()).collect::<Vec<_>>(), [slop_ninja_protocol::HUMAN_FPR_LOOSE_REASON]);
    // A share error of 0.2 on the mixed document fails the share gate.
    let mut share = ok.clone();
    share.share[4] = Some(600_000_000);
    assert_eq!(v3::gates_for_test(&plan, &share, &labels, 6)[0].gate, slop_ninja_protocol::Gate::Share);
    // Too few predictions with both role results fails attribution.
    let mut roles = ok;
    roles.roles = vec![false; 6];
    assert_eq!(v3::gates_for_test(&plan, &roles, &labels, 6)[0].gate, slop_ninja_protocol::Gate::Attribution);
}

#[test]
fn a_probe_build_follows_the_mix_by_largest_remainder() {
    let weak = super::plan::tests::artifact(super::plan::tests::identity(0, 0xb0), 0x22);
    let mut p = plan_v3(1, "testnet-probe-v1", 1024, weak);
    p.schema = super::plan::PROBE_PLAN_SCHEMA.into();
    p.reward_v1.as_mut().unwrap().probe_version = Some(1);
    p.validate(Ceilings { roster_cap: Some(40), accept_reward_v0: false }).unwrap();
    assert_eq!(p.counts(), [205, 563, 256]);
    // A cohort batch must realize the mix exactly.
    p.schema = super::plan::PLAN_SCHEMA_V3.into();
    p.reward_v1.as_mut().unwrap().probe_version = None;
    p.reward_v1.as_mut().unwrap().probes =
        vec![super::plan::ProbeRef { version: 1, size: 1024, key: "probes/v1/record.json".into(), sha256: hex::encode([1; 32]) }];
    assert!(p.validate(Ceilings { roster_cap: Some(40), accept_reward_v0: false }).unwrap_err().0.starts_with("mix"));
}

/// Exposed stock is drawn again under the reuse limit and gap: every parent
/// use records its generator, voice and operation, and a reused passage gets
/// a fresh one of each; canary audit humans join the ledger; the gap counts
/// drawn cohorts whatever the operator's numbering; fresh stock is refused.
#[test]
fn v3_exposed_stock_is_reused_with_fresh_generators_voices_and_operations() {
    let db = Sqlite::open();
    sql::migrate(&db, &EVAL_COHORT_SCHEMA).unwrap();
    let mut sim = Sim::new(&db, cfg(), 2_000_000);
    let (pool, texts) = pool_of(40);
    sim.pool = pool;
    sim.texts = texts;
    let generators: Vec<String> = [
        "@cf/meta/llama-3.2-3b-instruct",
        "@cf/meta/llama-3.1-8b-instruct-fp8-fast",
        "@cf/mistral/mistral-7b-instruct-v0.1",
        "@cf/ibm-granite/granite-4.0-h-micro",
    ]
    .iter()
    .map(|g| (*g).to_owned())
    .collect();
    let weak = v3_artifact(&mut sim, super::plan::tests::identity(0, 0xb0), [1, 2, 3, 4, 5]);
    let mut probe = plan_v3(1, "testnet-probe-v1", 20, weak.clone());
    probe.schema = super::plan::PROBE_PLAN_SCHEMA.into();
    probe.generators = generators.clone();
    probe.reward_v1.as_mut().unwrap().probe_version = Some(1);
    probe.reward_v1.as_mut().unwrap().canary.draw_humans = 0;
    sim.predict = Some(Box::new(|_, kind, task| {
        let c = model_conf(1, kind, task, (35.0, 65.0, 70.0), 10.0);
        (c, c * 0.4)
    }));
    let pseq = open_cohort(&db, &probe, 10, sim.now);
    assert_eq!(sim.run(pseq, 3_000), "closed");
    let probe_ref = super::plan::ProbeRef {
        version: 1,
        size: 20,
        key: "probes/v1/record.json".into(),
        sha256: lane_logic::sha_hex(&sim.eval["probes/v1/record.json"]),
    };
    // Fresh stock (a pool the deployment does not list as exposed) is refused.
    let mut fresh = plan_v3(2, "testnet-rv1-fresh", 20, weak.clone());
    fresh.pool.pool_id = "pool-v2".into();
    fresh.reward_v1.as_mut().unwrap().probes = vec![probe_ref.clone()];
    let bytes = serde_json::to_vec(&fresh).unwrap();
    assert_eq!(tx(&db, |s| engine::queue(s, &cfg(), &bytes, sim.now)).unwrap_err().code(), engine::FRESH_STOCK_REFUSED);
    // Four cohorts at skipping cohort numbers, reuse gap 1, limit 4.
    let mut epoch = 20;
    let mut drawn_humans = 0u64;
    for (k, cohort) in [3u64, 7, 8, 15, 16, 30].into_iter().enumerate() {
        let mut p = plan_v3(cohort, &format!("testnet-rv1-x{k}"), 20, weak.clone());
        p.generators = generators.clone();
        {
            let v = p.reward_v1.as_mut().unwrap();
            v.probes = vec![probe_ref.clone()];
            v.reuse = super::plan::Reuse { max_uses: 4, min_gap: 1 };
        }
        let seq = open_cohort(&db, &p, epoch, sim.now);
        epoch = run_to_close(&mut sim, &db, seq, epoch + 2) + 2;
        assert_eq!(fault_of(&db, seq), None);
        let draw: Value =
            serde_json::from_str(&db.conn.query_row("SELECT draw FROM cohorts WHERE seq = ?1", [seq], |r| r.get::<_, String>(0)).unwrap())
                .unwrap();
        assert_eq!(draw["draw_mode"], "exposed");
        assert_eq!(draw["reuse_ordinal"], json!(k as u64 + 1), "the gap counts drawn cohorts");
        drawn_humans += draw["humans"].as_u64().unwrap() + draw["audit_a"].as_u64().unwrap() + draw["audit_b"].as_u64().unwrap();
    }
    // Humans were drawn again across cohorts (never twice in one cohort).
    let (humans, distinct): (i64, i64) = db
        .conn
        .query_row(
            "SELECT COUNT(*), COUNT(DISTINCT pool_row) FROM docs
             WHERE kind = 'human_only' AND docset IN ('batch', 'audit_a', 'audit_b') AND pool_row IS NOT NULL
               AND seq <> ?1",
            [pseq],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(humans as u64, drawn_humans);
    assert!(humans > distinct, "exposed rows are drawn again ({humans} drawn, {distinct} distinct)");
    let per_cohort_dupes: i64 = db
        .conn
        .query_row(
            "SELECT COUNT(*) FROM (SELECT seq, pool_row FROM docs WHERE pool_row IS NOT NULL
             GROUP BY seq, pool_row HAVING COUNT(*) > 1 AND SUM(docset <> 'source') > 1)",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(per_cohort_dupes, 0, "never twice in one cohort");
    // Every parent use recorded its voice and operation (and, once accepted,
    // its generator); none repeats on one passage within the limit.
    let rows: Vec<(String, i64, String, String, String)> = db
        .conn
        .prepare("SELECT row_id, uses, generators, voices, operations FROM reuse_sources")
        .unwrap()
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    let list = |s: &str| s.split(',').filter(|x| !x.is_empty()).map(str::to_owned).collect::<Vec<_>>();
    let mut reused = 0;
    for (row, uses, g, v, o) in &rows {
        let (g, v, o) = (list(g), list(v), list(o));
        assert_eq!(v.len() as i64, *uses, "{row}: one voice per use");
        assert_eq!(o.len() as i64, *uses, "{row}: one operation per use");
        assert!(g.len() as i64 <= *uses, "{row}: at most one generator per use");
        let distinct = |x: &Vec<String>| x.iter().collect::<BTreeSet<_>>().len() == x.len();
        assert!(distinct(&v) && distinct(&o) && distinct(&g), "{row}: a fresh generator, voice and operation each use");
        if *uses >= 2 {
            reused += 1;
        }
    }
    assert!(reused > 0, "some passage parented more than once");
    // Canary audit humans join the ledger after their cohort.
    let audit_rows: Vec<String> = db
        .conn
        .prepare("SELECT DISTINCT pool_row FROM docs WHERE docset IN ('audit_a', 'audit_b')")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    assert!(!audit_rows.is_empty());
    for r in &audit_rows {
        assert!(rows.iter().any(|(id, ..)| id == r), "audit human {r} is in the reuse ledger");
    }
}

fn refused_plan(p: &CohortPlan, c: Ceilings) -> &'static str {
    p.validate(c).unwrap_err().0
}

/// `engine::SUMMARY_FIXED_BYTES` (and so `arena_routes::summary_bytes`)
/// holds the widest summary the native tests build: every task kind, 64 AI
/// profiles with 96-byte names, both role profiles on every task, and the
/// widest per-task values.
#[test]
fn the_summary_bound_holds_the_widest_summary() {
    let kinds = ["human_only", "ai_only", "mixed", "rewrite", "human_with_ai_edit", "ai_with_human_edit"];
    let tasks = 2_224u32;
    let rows: Vec<engine::PredRow> = (0..tasks)
        .map(|t| engine::PredRow {
            task: t,
            set: if t < 1_000 { "batch".into() } else { "probe".into() },
            kind: kinds[t as usize % kinds.len()].into(),
            label_share_ppb: 1_000_000_000,
            outcome: 1,
            inv_ppb: Some(1_000_000_000),
            share_ppb: Some(1_000_000_000),
            human: Some(format!("{:0>96}", t % 64)),
            ai: Some(format!("{:0>96}", t % 64)),
        })
        .collect();
    let s = engine::summarize_job(i64::MAX, &"f".repeat(64), &rows).expect("summary");
    let bytes = serde_json::to_vec(&s).expect("json").len();
    let per_task = crate::arena_routes::SUMMARY_TASK_BYTES * tasks as usize;
    assert!(bytes <= crate::arena_routes::summary_bytes(u64::from(tasks)), "summary {bytes} B over its bound");
    assert!(bytes - per_task.min(bytes) <= engine::SUMMARY_FIXED_BYTES / 2, "fixed part {} B leaves less than half the bound as margin", bytes - per_task.min(bytes));
}

// ------------------------------------------------- chain-record restart

/// Every row of every table of a cohort store, for before/after comparisons.
fn dump(db: &Sqlite) -> Vec<(String, Vec<String>)> {
    let tables: Vec<String> = db
        .conn
        .prepare("SELECT name FROM sqlite_master WHERE type = 'table' ORDER BY name")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    tables
        .into_iter()
        .map(|t| {
            let mut stmt = db.conn.prepare(&format!("SELECT * FROM \"{t}\"")).unwrap();
            let n = stmt.column_count();
            let mut rows: Vec<String> = stmt
                .query_map([], |r| {
                    Ok((0..n).map(|i| format!("{:?}", r.get_ref(i).unwrap())).collect::<Vec<_>>().join("|"))
                })
                .unwrap()
                .map(Result::unwrap)
                .collect();
            rows.sort();
            (t, rows)
        })
        .collect()
}

fn open_with(db: &Sqlite, c: &EvalConfig, p: &CohortPlan, first_epoch: u64, now: u64) -> i64 {
    let bytes = serde_json::to_vec(p).unwrap();
    let seq = tx(db, |s| engine::queue(s, c, &bytes, now)).unwrap();
    kick_at(db, first_epoch, now);
    kick_at(db, first_epoch + 1, now);
    kick_at(db, first_epoch + 2, now);
    seq
}

fn refused_queue(db: &Sqlite, c: &EvalConfig, p: &CohortPlan) -> String {
    let bytes = serde_json::to_vec(p).unwrap();
    tx(db, |s| engine::queue(s, c, &bytes, 0)).unwrap_err().code().to_owned()
}

/// The chain-record restart (w2-golive PLAN 6.2): a named main authority is a
/// fresh store, so its reference chain starts at R_0 with no predecessor, and
/// its published index, fingerprints, probe versions and reuse ledger start
/// empty. It shares the buckets and the lane object namespace with the
/// original authority, and writes nothing the original wrote: its probe
/// objects sit under its own prefix, its labels under its own prefix, its
/// cohort numbers above the original's, and its lane objects are new, so no
/// lane holding the original's later `(seq, gen)` refuses its kicks. The
/// original authority's store and objects are byte for byte as they were.
#[test]
fn a_named_authority_restarts_the_chain_record_and_leaves_the_original_untouched() {
    let id = super::plan::tests::identity;
    // The original authority: a probe build and one cohort on the rehearsal reference.
    let old_db = Sqlite::open();
    let (mut old_sim, weak, probe_ref) = probe_built(&old_db);
    let mut a = plan_v3(2, "testnet-rv1-a", 20, weak.clone());
    a.reward_v1.as_mut().unwrap().probes = vec![probe_ref.clone()];
    let seq_a = open_cohort(&old_db, &a, 20, old_sim.now);
    run_to_close(&mut old_sim, &old_db, seq_a, 22);
    let old_status = status(&old_db, old_sim.now);
    let old_rows = dump(&old_db);
    let old_eval = old_sim.eval.clone();
    let old_rcpt = old_sim.rcpt.clone();
    let old_lanes: BTreeSet<String> = old_sim.lanes.keys().cloned().collect();
    assert!(old_lanes.iter().all(|n| n.contains("/testnet/main/") && !n.contains("/authority/")), "{old_lanes:?}");
    assert!(old_eval.keys().any(|k| k == "probes/v1/record.json"));

    // The new authority follows the original: no original label has its
    // prefix and every original cohort number is below its first cohort.
    let earlier: Vec<(String, u64)> = old_status["cohorts"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| (c["label"].as_str().unwrap().to_owned(), c["cohort"].as_u64().unwrap()))
        .collect();
    let follows = |a: &crate::config::Authority| a.follows(earlier.iter().map(|(l, c)| (l.as_str(), *c)));
    let authority = crate::config::Authority::named("r0b", 3).unwrap();
    assert_eq!(follows(&authority), Ok(()));
    assert!(follows(&crate::config::Authority::named("r0b", 2).unwrap()).is_err());
    assert!(follows(&crate::config::Authority::named("rv1", 3).unwrap()).is_err());

    // A fresh store (a new Durable Object) sharing the buckets and the lane
    // objects; heartbeats now reach only it.
    let new_db = Sqlite::open();
    sql::migrate(&new_db, &EVAL_COHORT_SCHEMA).unwrap();
    let ncfg = EvalConfig { authority: authority.clone(), ..cfg() };
    assert_eq!(ncfg.lane_scope(), "main/authority/r0b");
    assert_eq!(ncfg.kick_authority().as_deref(), Some("r0b"));
    let mut sim = Sim::new(&new_db, ncfg.clone(), old_sim.now + 60_000);
    sim.pool = old_sim.pool.clone();
    sim.texts = old_sim.texts.clone();
    sim.eval = std::mem::take(&mut old_sim.eval);
    sim.rcpt = std::mem::take(&mut old_sim.rcpt);
    sim.lanes = std::mem::take(&mut old_sim.lanes);
    // The original's lane objects call back only the original authority,
    // which this simulation does not run on: none of them runs here.
    for l in sim.lanes.values_mut() {
        l.armed = false;
    }
    // Every kick reaches its lane twice.
    sim.duplicate = true;
    let r0 = v3_artifact(&mut sim, id(0, 0xb0), [1, 2, 3, 8, 5]);
    let r0_content = sim.artifact_digests(&r0).content.0;
    assert_ne!(r0_content, sim.artifact_digests(&weak).content.0);
    // Nothing reaches the new authority under the original's labels or numbers.
    let mut probe = plan_v3(3, "testnet-rv1-probe-v1", 20, r0.clone());
    probe.schema = super::plan::PROBE_PLAN_SCHEMA.into();
    probe.reward_v1.as_mut().unwrap().probe_version = Some(1);
    probe.reward_v1.as_mut().unwrap().canary.draw_humans = 0;
    assert!(refused_queue(&new_db, &ncfg, &probe).starts_with("label"), "the original's label prefix");
    probe.label = "testnet-r0b-probe-v1".into();
    probe.cohort = 2;
    assert!(refused_queue(&new_db, &ncfg, &probe).starts_with("cohort number"), "below the first cohort");
    // A retired authority takes no plan at all.
    let retired = EvalConfig { authority: crate::config::Authority::retired("main"), ..cfg() };
    let mut later = a.clone();
    later.cohort = 9;
    later.label = "testnet-rv1-z".into();
    assert!(refused_queue(&old_db, &retired, &later).contains("retired"));

    // The restart's probe build: R_0 with no predecessor, published and
    // fingerprinted under the authority's own probe keys.
    probe.cohort = 3;
    sim.predict = Some(Box::new(|_, kind, task| {
        let c = model_conf(1, kind, task, (35.0, 65.0, 70.0), 10.0);
        (c, c * 0.4)
    }));
    let pseq = open_with(&new_db, &ncfg, &probe, 40, sim.now);
    assert_eq!(pseq, 1, "the new authority's first sequence number");
    assert_eq!(sim.run(pseq, 3_000), "closed", "fault {:?}", fault_of(&new_db, pseq));
    let regime = tx(&new_db, |s| v3::regime(s, &ncfg.chain)).unwrap();
    assert_eq!(regime["references"]["k"], 0);
    assert_eq!(regime["references"]["current_content"], r0_content.to_string());
    assert_eq!(regime["references"]["predecessor_content"], Value::Null);
    assert_eq!(regime["launch_burn"], true, "nothing has activated on the restarted chain");
    let (index, vkeys) = tx(&new_db, v3::published_index).unwrap();
    assert_eq!(index.contents.contents, vec![r0_content]);
    assert_eq!(index.fingerprints.entries.len(), 1);
    assert!(vkeys.values().all(|k| k.starts_with("probes/authority/r0b/v1/fp/")), "{vkeys:?}");
    let record_key = "probes/authority/r0b/v1/record.json";
    let record = sim.eval[record_key].clone();
    // Every object the original authority wrote is still there, unchanged.
    for (k, v) in &old_eval {
        assert_eq!(sim.eval.get(k), Some(v), "eval object {k}");
    }
    for (k, v) in &old_rcpt {
        assert_eq!(sim.rcpt.get(k), Some(v), "receipts object {k}");
    }
    // Only new lane objects ran the new authority's work.
    let new_lanes: BTreeSet<String> = sim.lanes.keys().filter(|n| !old_lanes.contains(*n)).cloned().collect();
    assert!(!new_lanes.is_empty());
    assert!(new_lanes.iter().all(|n| n.contains("/testnet/main/authority/r0b/")), "{new_lanes:?}");
    let stored: Vec<String> = new_db
        .conn
        .prepare("SELECT DISTINCT name FROM lanes")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    assert!(stored.iter().all(|n| new_lanes.contains(n)), "{stored:?}");

    // A cohort on the restarted chain: R_0 alone, the authority's probe
    // version, its own label and number.
    let probe_new =
        super::plan::ProbeRef { version: 1, size: 20, key: record_key.into(), sha256: lane_logic::sha_hex(&record) };
    let mut b = plan_v3(4, "testnet-r0b-a", 20, r0.clone());
    b.reward_v1.as_mut().unwrap().probes = vec![probe_new];
    let seq_b = open_with(&new_db, &ncfg, &b, 50, sim.now);
    run_to_close(&mut sim, &new_db, seq_b, 52);
    let pb = sim.packages_v3.pop().unwrap();
    assert_eq!((pb.manifest.cohort, pb.manifest.cohort_label.as_str()), (4, "testnet-r0b-a"));
    assert_eq!(pb.manifest.references.index, 0);
    assert_eq!(pb.manifest.references.current.content, r0_content);
    assert_eq!(pb.manifest.references.predecessor, None);
    assert_eq!(pb.package.mode, slop_ninja_protocol::CutoffMode::NoTarget);
    // The restarted chain's objects never land under the original's labels.
    assert!(
        sim.rcpt
            .keys()
            .filter(|k| !old_rcpt.contains_key(*k))
            .all(|k| k.starts_with("cohorts/testnet-r0b-") || k.starts_with("probes/authority/r0b/")),
        "{:?}",
        sim.rcpt.keys().collect::<Vec<_>>()
    );
    for (k, v) in &old_eval {
        assert_eq!(sim.eval.get(k), Some(v), "eval object {k}");
    }
    for (k, v) in &old_rcpt {
        assert_eq!(sim.rcpt.get(k), Some(v), "receipts object {k}");
    }

    // The new authority holds only its own probe version: a plan naming the
    // original's record would fail the seal's check (`probe record digest`).
    let versions: Vec<(i64, String, String)> = new_db
        .conn
        .prepare("SELECT version, record_key, record_sha256 FROM probe_versions")
        .unwrap()
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    assert_eq!(versions, vec![(1, record_key.to_owned(), lane_logic::sha_hex(&record))]);
    assert_ne!(lane_logic::sha_hex(&record), probe_ref.sha256);

    // The original authority's store is exactly as it was.
    assert_eq!(dump(&old_db), old_rows);
}

/// The deployment's main-authority stores, addressed the way the Worker and
/// the cohort objects route (`config`'s pure routing): a heartbeat goes to
/// each authority `kicked_authorities` names, and the object takes it only
/// under its own recorded identity (`object_identity`).
struct Objects {
    stores: BTreeMap<String, (Sqlite, Option<String>)>,
}

impl Objects {
    fn new(names: &[&str]) -> Self {
        let stores = names
            .iter()
            .map(|n| {
                let db = Sqlite::open();
                sql::migrate(&db, &EVAL_COHORT_SCHEMA).unwrap();
                ((*n).to_owned(), (db, None))
            })
            .collect();
        Self { stores }
    }

    /// One heartbeat under `deployed`: a delivery (`tick` false) or a tick.
    /// Returns the authorities whose objects took it.
    fn heartbeat(&mut self, deployed: &crate::config::Authority, tick: bool, index: u64, now: u64) -> Vec<String> {
        let mut took = Vec::new();
        for name in crate::config::kicked_authorities(deployed, tick, false) {
            let (db, stored) = self.stores.get_mut(&name).expect("a known authority's object");
            let (id, write) = crate::config::object_identity(stored.as_deref(), &name, true).unwrap();
            if write {
                *stored = Some(id);
            }
            kick_at(db, index, now);
            took.push(name);
        }
        took
    }

    fn db(&self, name: &str) -> &Sqlite {
        &self.stores[name].0
    }
}

/// Every table of a store but its observation row.
fn dump_but_observation(db: &Sqlite) -> Vec<(String, Vec<String>)> {
    dump(db).into_iter().filter(|(t, _)| t != "eval_obs").collect()
}

/// Heartbeats across the switch, routed as the Worker routes them: before
/// it only the original takes them; after it every delivery reaches only
/// the named authority, ticks reach the original too (so work it still held
/// would finish), duplicate deliveries and ticks for the same block credit
/// one tempo once, and the original's cohorts and every other table are
/// unchanged. An old-version kick (no authority header) or a lane callback
/// without an authority reaches only the original; the named object refuses
/// another authority's request.
#[test]
fn wakeups_and_callbacks_across_the_switch_reach_only_their_own_authority() {
    use crate::config::{Authority, callback_authority, object_identity, own_authority};
    let mut objects = Objects::new(&["main", "r0b"]);
    let original = Authority::default();
    assert_eq!(objects.heartbeat(&original, false, 30, 1_000_000), vec!["main"]);
    assert_eq!(objects.heartbeat(&original, true, 31, 1_001_000), vec!["main"]);
    let before = dump_but_observation(objects.db("main"));
    let named = Authority::named("r0b", 13).unwrap();
    for now in [1_002_000, 1_002_000] {
        assert_eq!(objects.heartbeat(&named, false, 32, now), vec!["r0b"]);
    }
    for now in [1_002_500, 1_002_500] {
        assert_eq!(objects.heartbeat(&named, true, 32, now), vec!["r0b", "main"]);
        heartbeat(objects.db("r0b"), now);
    }
    let o = tx(objects.db("r0b"), engine::observed).unwrap();
    assert_eq!((o.active_tempos, o.uncredited_tempos, o.epoch.unwrap().index), (0, 0, 32));
    for now in [1_003_000, 1_003_000] {
        objects.heartbeat(&named, false, 33, now);
    }
    objects.heartbeat(&named, true, 33, 1_003_100);
    let o = tx(objects.db("r0b"), engine::observed).unwrap();
    assert_eq!((o.active_tempos, o.uncredited_tempos, o.epoch.unwrap().index), (1, 0, 33));
    assert_eq!(o.kicks, 9);
    assert_eq!(dump_but_observation(objects.db("main")), before, "the original holds the same cohorts");
    // A kick from a Worker version before named authorities carries no
    // authority: it names the original, which the named object refuses.
    assert_eq!(object_identity(Some("r0b"), callback_authority(None), true), Err("authority_mismatch"));
    assert_eq!(object_identity(Some("main"), callback_authority(None), true), Ok(("main".into(), false)));
    // Lanes report to the authority that kicked them.
    let kick_cfg = EvalConfig { authority: named.clone(), ..cfg() };
    assert_eq!(callback_authority(kick_cfg.kick_authority().as_deref()), "r0b");
    assert_eq!(callback_authority(cfg().kick_authority().as_deref()), "main");
    // An audit read or a supersede call creates nothing.
    assert_eq!(object_identity(None, "r0c", false), Err("unknown_authority"));
    // A deploy that reverts the vars: the named object keeps its lane names
    // and admits nothing; the original, superseded, admits nothing either.
    let r0b_after = own_authority(Some(&original), "r0b", None);
    assert!(r0b_after.is_retired());
    assert_eq!(r0b_after.lane_scope("main"), named.lane_scope("main"));
    assert!(r0b_after.admits("testnet-r0b-20", 20).is_err());
    let main_after = own_authority(Some(&original), "main", Some("r0b"));
    assert!(main_after.is_retired() && main_after.superseded_by() == Some("r0b"));
    assert!(main_after.admits("testnet-rv1-g", 13).is_err());
    // Not superseded, the original is current again under a revert.
    assert!(!own_authority(Some(&original), "main", None).is_retired());
}

/// The original authority's configuration once the deployment names a
/// successor: retired, as the supersede route reads it.
fn predecessor_cfg() -> EvalConfig {
    let r0b = crate::config::Authority::named("r0b", 13).unwrap();
    EvalConfig { authority: crate::config::own_authority(Some(&r0b), "main", None), ..cfg() }
}

/// Succession is durable and needs an idle predecessor: a predecessor
/// holding an open cohort refuses (nothing written), one whose labels or
/// numbers the successor's rule would share refuses, and an idle one records
/// its first successor once. A superseded authority admits no plan, even
/// when a reverted deployment names it again, and its calendar is paused.
#[test]
fn succession_needs_an_idle_predecessor_and_survives_a_reverted_deploy() {
    use crate::config::{Authority, Supersede, own_authority};
    let retired = predecessor_cfg();
    let busy = Sqlite::open();
    sql::migrate(&busy, &EVAL_COHORT_SCHEMA).unwrap();
    let mut p = plan(10);
    p.label = "testnet-rv1-x".into();
    open_cohort(&busy, &p, 10, 1_000_000);
    let rows = dump(&busy);
    let req = Supersede::of(&Authority::named("r0b", 13).unwrap()).unwrap();
    let r = tx(&busy, |s| super::succession::supersede(s, &retired, &req, 1_000_000)).unwrap();
    assert_eq!(r, Err("predecessor_busy"));
    assert_eq!(dump(&busy), rows, "a refusal writes nothing");
    assert_eq!(tx(&busy, super::succession::superseded_by).unwrap(), None);

    let db = Sqlite::open();
    let (sim, _, _) = probe_built(&db);
    let highest = status(&db, sim.now)["cohorts"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["cohort"].as_u64().unwrap())
        .max()
        .unwrap();
    // A first cohort at or below the predecessor's numbers, or a prefix one
    // of its labels has, is refused.
    for bad in [Authority::named("r0b", highest).unwrap(), Authority::named("probe", highest + 1).unwrap()] {
        let req = Supersede::of(&bad).unwrap();
        assert_eq!(
            tx(&db, |s| super::succession::supersede(s, &retired, &req, sim.now)).unwrap(),
            Err("predecessor_numbers")
        );
    }
    let req = Supersede::of(&Authority::named("r0b", highest + 1).unwrap()).unwrap();
    let v = tx(&db, |s| super::succession::supersede(s, &retired, &req, sim.now)).unwrap().unwrap();
    assert_eq!(v["superseded_by"], "r0b");
    // Idempotent; a later successor leaves the first recorded.
    tx(&db, |s| super::succession::supersede(s, &retired, &req, sim.now)).unwrap().unwrap();
    let later = Supersede::of(&Authority::named_after("r0c", highest + 5, &["r0b"]).unwrap()).unwrap();
    let v = tx(&db, |s| super::succession::supersede(s, &retired, &later, sim.now)).unwrap().unwrap();
    assert_eq!(v["superseded_by"], "r0b");
    assert_eq!(tx(&db, super::succession::superseded_by).unwrap().as_deref(), Some("r0b"));
    // A reverted deployment names the original again: superseded, it admits
    // nothing, and its calendar opens nothing.
    let superseded = tx(&db, super::succession::superseded_by).unwrap();
    let reverted =
        EvalConfig { authority: own_authority(Some(&Authority::default()), "main", superseded.as_deref()), ..cfg() };
    let mut q = plan(10);
    q.cohort = highest + 1;
    q.label = "testnet-r0b-collide".into();
    let bytes = serde_json::to_vec(&q).unwrap();
    assert!(matches!(engine::admit(&reverted, &bytes), Err(engine::EvalError::Plan(m)) if m.contains("superseded")));
    assert!(tx(&db, |s| super::calendar::due(s, &reverted)).unwrap().is_none());
}

/// A predecessor records no successor while it still has work: the Worker
/// kicks it only until succession completes, so a closure, a spend
/// settlement or a purge of private results left then would never run.
/// Each refuses `predecessor_work` (writing nothing) until it is done; the
/// predecessor then records its successor and has no work.
#[test]
fn a_predecessor_with_closure_settle_or_purge_work_records_no_successor() {
    use crate::config::{Authority, Supersede};
    let retired = predecessor_cfg();
    let db = Sqlite::open();
    sql::migrate(&db, &EVAL_COHORT_SCHEMA).unwrap();
    let mut p = plan(10);
    p.label = "testnet-rv1-x".into();
    let seq = open_cohort(&db, &p, 10, 1_000_000);
    let req = Supersede::of(&Authority::named("r0b", p.cohort + 1).unwrap()).unwrap();
    let set = |columns: &str| {
        db.conn.execute(&format!("UPDATE cohorts SET {columns} WHERE seq = ?1"), rusqlite::params![seq]).unwrap();
    };
    let done = "reserved_microusd = 1, settled_microusd = 1, purged_ms = 1, v3 = NULL, closure_digest = NULL";
    for (work, columns) in [
        ("settle", "settled_microusd = NULL"),
        ("purge", "purged_ms = NULL"),
        ("closure", "v3 = '{}', package_digest = 'aa', closure_digest = NULL"),
    ] {
        set(&format!("phase = 'closed', {done}"));
        set(columns);
        assert!(tx(&db, |s| engine::has_work(s, &retired)).unwrap(), "{work}");
        let rows = dump(&db);
        let r = tx(&db, |s| super::succession::supersede(s, &retired, &req, 1_000_000)).unwrap();
        assert_eq!(r, Err("predecessor_work"), "{work}");
        assert_eq!(dump(&db), rows, "{work}: a refusal writes nothing");
    }
    set(&format!("phase = 'closed', {done}"));
    assert!(!tx(&db, |s| engine::has_work(s, &retired)).unwrap());
    let v = tx(&db, |s| super::succession::supersede(s, &retired, &req, 1_000_000)).unwrap().unwrap();
    assert_eq!(v["superseded_by"], "r0b");
    assert!(!tx(&db, |s| engine::has_work(s, &retired)).unwrap(), "nothing left for kicks that stop");
}

/// A superseded authority queues nothing, even with the configuration it
/// read before a successor's supersede committed (an operator plan awaits the
/// ledger, and a calendar open its predecessors, between its admit and its
/// queue): the queueing transaction reads the succession itself.
#[test]
fn a_plan_admitted_before_a_supersede_never_queues_after_it() {
    use crate::config::{Authority, Supersede};
    let retired = predecessor_cfg();
    let db = Sqlite::open();
    sql::migrate(&db, &EVAL_COHORT_SCHEMA).unwrap();
    let c = cfg();
    let p = plan(10);
    let bytes = serde_json::to_vec(&p).unwrap();
    engine::admit(&c, &bytes).unwrap();
    // The successor's first cohort is this plan's number.
    let req = Supersede::of(&Authority::named("r0b", p.cohort.max(1)).unwrap()).unwrap();
    tx(&db, |s| super::succession::supersede(s, &retired, &req, 1_000_000)).unwrap().unwrap();
    let r = tx(&db, |s| engine::queue(s, &c, &bytes, 1_000_000));
    assert!(matches!(r, Err(engine::EvalError::Plan(m)) if m.contains("superseded")), "{r:?}");
    assert!(tx(&db, engine::cohorts).unwrap().is_empty());
}

/// The named authority's own configuration routes its probe objects, lanes
/// and calendar numbers; the audit and operator routes accept only known
/// authorities, and every predecessor is known.
#[test]
fn the_authority_routes_parse_and_know_only_the_deployments_authorities() {
    use crate::config::{Authority, authority_route};
    assert_eq!(authority_route("/eval/authority/main/status"), Some(("main", "status")));
    assert_eq!(authority_route("/eval/authority/r0b/abandon"), Some(("r0b", "abandon")));
    assert_eq!(authority_route("/eval/authority/r0b/clear"), Some(("r0b", "clear")));
    for bad in
        ["/eval/authority/load/status", "/eval/authority/R0B/status", "/eval/authority/r0b/plan", "/eval/authority/r0b"]
    {
        assert_eq!(authority_route(bad), None, "{bad}");
    }
    let a = Authority::named_after("r0c", 20, &["r0b"]).unwrap();
    assert_eq!(a.predecessors(), vec!["main", "r0b"]);
    assert!(a.known("main") && a.known("r0b") && a.known("r0c") && !a.known("r0x"));
    assert_eq!(crate::config::kicked_authorities(&a, false, false), vec!["r0c"]);
    assert_eq!(crate::config::kicked_authorities(&a, true, false), vec!["r0c", "main", "r0b"]);
    assert_eq!(crate::config::kicked_authorities(&a, true, true), vec!["r0c"]);
    let original = Authority::default();
    assert!(original.known("main") && !original.known("r0b"));
    assert!(original.predecessors().is_empty());
}

/// A plan that names an artifact spec binds the v2 limits record (its id and
/// frozen digest) and publishes it in `components.json`; a plan that names
/// none keeps the v1 record, unpublished, so earlier digests are unchanged.
#[test]
fn a_plan_naming_a_spec_binds_and_publishes_the_v2_limits_record() {
    let v3_sha = slop_ninja_artifact_check::spec::Version::V3.spec_sha256();
    let mut p = plan(10);
    let (j, domain) = v3::limits_record(&p).unwrap();
    assert_eq!((j["schema"].as_str(), domain), (Some("slop_ninja_eval_limits_v1"), v3::LIMITS_DOMAIN_V1));
    assert!(j.get("artifact_spec").is_none() && j.get("step_budget_ms").is_some());
    assert_eq!(v3::published_limits(&p).unwrap(), None);
    let s = &mut p.fanout.as_mut().unwrap().score;
    s.artifact_max_mib = None;
    s.artifact_spec = Some("slop_ninja_a_artifact_spec_v3".into());
    s.load_ms_per_mib = Some(50);
    s.artifact_spec_sha256 = Some(v3_sha.clone());
    let (j, domain) = v3::limits_record(&p).unwrap();
    assert_eq!((j["schema"].as_str(), domain), (Some("slop_ninja_eval_limits_v2"), v3::LIMITS_DOMAIN_V2));
    assert_eq!(j["artifact_spec"], json!({"id": "slop_ninja_a_artifact_spec_v3", "sha256": v3_sha}));
    assert!(j.get("step_budget_ms").is_none() && j["step_wall_ms"].is_u64());
    assert_eq!(v3::published_limits(&p).unwrap(), Some(j));
    // A plan that names its spec only in the admission block (stored before
    // `fanout.score.artifact_spec_sha256` existed) keeps the v1 record it
    // bound at seal under the earlier build, though its loads name the spec.
    let mut stored = p.clone();
    stored.fanout.as_mut().unwrap().score.artifact_spec_sha256 = None;
    stored.admission = Some(crate::eval::plan::AdmissionPlan {
        artifact_spec: "slop_ninja_a_artifact_spec_v3".into(),
        artifact_spec_sha256: v3_sha.clone(),
        roster_cap: 2,
        selection: "earliest_commitment/v1".into(),
        part_size: 5 << 20,
        max_parts: 100,
        request_validity_blocks: 360,
        operator_hotkeys: Vec::new(),
        operator_coldkeys: Vec::new(),
        fresh_pool: None,
    });
    assert_eq!(stored.load_spec_sha256(), Some(v3_sha.as_str()));
    let (j, domain) = v3::limits_record(&stored).unwrap();
    assert_eq!((j["schema"].as_str(), domain), (Some("slop_ninja_eval_limits_v1"), v3::LIMITS_DOMAIN_V1));
    assert_eq!(v3::published_limits(&stored).unwrap(), None);
}

/// The cohort's artifact spec reaches every scoring lane's assignment as the
/// digest its loads name; a plan that names none sends none.
#[test]
fn a_cohort_naming_an_artifact_spec_hands_its_digest_to_every_scoring_lane() {
    let db = Sqlite::open();
    sql::migrate(&db, &EVAL_COHORT_SCHEMA).unwrap();
    let mut p = plan(10);
    p.label = "spec-v3".into();
    let v3_sha = slop_ninja_artifact_check::spec::Version::V3.spec_sha256();
    p.fanout.as_mut().unwrap().score.artifact_max_mib = None;
    p.fanout.as_mut().unwrap().score.artifact_spec = Some("slop_ninja_a_artifact_spec_v3".into());
    p.fanout.as_mut().unwrap().score.load_ms_per_mib = Some(50);
    p.fanout.as_mut().unwrap().score.artifact_spec_sha256 = Some(v3_sha.clone());
    let mut sim = Sim::new(&db, cfg(), 1_000_000);
    let seq = open_cohort(&db, &p, 10, sim.now);
    sim.frozen = vec!["score"];
    for _ in 0..400 {
        sim.tick();
        if phase(&db, seq) == "scoring" {
            break;
        }
    }
    assert_eq!(phase(&db, seq), "scoring");
    sim.tick();
    let scoring: Vec<String> = sim.lanes.iter().filter(|(_, l)| l.kind == "score").map(|(n, _)| n.clone()).collect();
    assert!(!scoring.is_empty());
    for name in &scoring {
        let a = sim.report(&sim.lane_ref(name), "starting", None);
        assert_eq!(a.spec_sha256.as_deref(), Some(v3_sha.as_str()), "{name}");
    }
    // Unnamed: nothing is sent.
    let db2 = Sqlite::open();
    sql::migrate(&db2, &EVAL_COHORT_SCHEMA).unwrap();
    let mut q = plan(10);
    q.label = "spec-none".into();
    let mut sim2 = Sim::new(&db2, cfg(), 1_000_000);
    let seq2 = open_cohort(&db2, &q, 10, sim2.now);
    sim2.frozen = vec!["score"];
    for _ in 0..400 {
        sim2.tick();
        if phase(&db2, seq2) == "scoring" {
            break;
        }
    }
    sim2.tick();
    let name = sim2.lanes.iter().find(|(_, l)| l.kind == "score").map(|(n, _)| n.clone()).unwrap();
    assert_eq!(sim2.report(&sim2.lane_ref(&name), "starting", None).spec_sha256, None);
    // A spec the scorer does not admit loads by is refused at queue time.
    let mut bad = plan(10);
    bad.label = "spec-v1".into();
    bad.fanout.as_mut().unwrap().score.artifact_max_mib = None;
    bad.fanout.as_mut().unwrap().score.artifact_spec = Some("slop_ninja_a_artifact_spec_v1".into());
    assert!(refused_plan(&bad, Ceilings { roster_cap: Some(40), accept_reward_v0: true }).contains("artifact_spec"));
    bad.fanout.as_mut().unwrap().score.artifact_spec = Some("slop_ninja_a_artifact_spec_v9".into());
    assert!(refused_plan(&bad, Ceilings { roster_cap: Some(40), accept_reward_v0: true }).contains("artifact_spec"));
}

// ------------------------------------------------------------ receipt retention

fn receipts_under(sim: &Sim<'_>, label: &str) -> usize {
    let p = format!("cohorts/{label}/receipts/");
    sim.rcpt.keys().filter(|k| k.starts_with(&p)).count()
}

fn retention_of(db: &Sqlite, seq: i64) -> (Option<i64>, Option<i64>, i64, i64) {
    db.conn
        .query_row(
            "SELECT receipts_due_after, receipts_deleted_ms, receipts_deleted_objects,
                (SELECT COUNT(*) FROM receipts WHERE seq = ?1) + (SELECT COUNT(*) FROM receipt_objects WHERE seq = ?1)
             FROM cohorts WHERE seq = ?1",
            [seq],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .unwrap()
}

/// The N+2 rule end to end: a probe build and three reward-v1 cohorts. Each
/// cohort's detailed receipts (chunk objects, bundles, receipt index, chunk
/// map and receipt rows; a probe build's index under its probe prefix) stay
/// until two later cohorts have completed, and are then deleted, while its
/// package, the receipt root and count and the cohort row stay.
#[test]
fn detailed_receipts_are_deleted_after_two_later_cohorts_complete() {
    let db = Sqlite::open();
    sql::migrate(&db, &EVAL_COHORT_SCHEMA).unwrap();
    let mut sim = Sim::new(&db, cfg(), 2_000_000);
    let (pool, texts) = pool_of(40);
    sim.pool = pool;
    sim.texts = texts;
    let weak = v3_artifact(&mut sim, super::plan::tests::identity(0, 0xb0), [1, 2, 3, 4, 5]);
    let mut probe = plan_v3(1, "testnet-probe-v1", 20, weak.clone());
    probe.schema = super::plan::PROBE_PLAN_SCHEMA.into();
    probe.reward_v1.as_mut().unwrap().probe_version = Some(1);
    probe.reward_v1.as_mut().unwrap().canary.draw_humans = 0;
    sim.predict = Some(Box::new(|_, kind, task| {
        let c = model_conf(1, kind, task, (35.0, 65.0, 70.0), 10.0);
        (c, c * 0.4)
    }));
    let pseq = open_cohort(&db, &probe, 10, sim.now);
    assert_eq!(sim.run(pseq, 3_000), "closed");
    sim.tick();
    let probe_ref = super::plan::ProbeRef {
        version: 1,
        size: 20,
        key: "probes/v1/record.json".into(),
        sha256: lane_logic::sha_hex(&sim.eval["probes/v1/record.json"]),
    };
    assert!(receipts_under(&sim, "testnet-probe-v1") > 0);
    assert!(sim.rcpt.contains_key("probes/v1/receipt-index.signed"));
    let mut epoch = 20;
    let mut done: Vec<(i64, String)> = vec![(pseq, "testnet-probe-v1".into())];
    let roots = |seq: i64| -> (Option<String>, Option<i64>) {
        db.conn
            .query_row("SELECT receipt_root, receipt_count FROM cohorts WHERE seq = ?1", [seq], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap()
    };
    let probe_roots = roots(pseq);
    for (k, cohort) in [2u64, 3, 4].into_iter().enumerate() {
        let label = format!("testnet-rv1-r{k}");
        let mut p = plan_v3(cohort, &label, 20, weak.clone());
        p.reward_v1.as_mut().unwrap().probes = vec![probe_ref.clone()];
        let seq = open_cohort(&db, &p, epoch, sim.now);
        epoch = run_to_close(&mut sim, &db, seq, epoch + 2) + 2;
        for _ in 0..3 {
            sim.tick();
        }
        assert_eq!(fault_of(&db, seq), None);
        assert!(receipts_under(&sim, &label) > 0, "{label}: its own receipts stay");
        done.push((seq, label));
        // Every cohort with two later completed cohorts is deleted; the two
        // newest completed cohorts are retained.
        let completed = done.len();
        for (j, (s, l)) in done.iter().enumerate() {
            let (due, deleted, objects, rows) = retention_of(&db, *s);
            if j + 2 < completed {
                assert_eq!(due, Some(cohort as i64 - (completed - j - 3) as i64), "{l}: due after N+2");
                assert!(deleted.is_some(), "{l}: deleted");
                assert!(objects > 0, "{l}: objects counted");
                assert_eq!(rows, 0, "{l}: receipt rows deleted");
                assert_eq!(receipts_under(&sim, l), 0, "{l}: chunk objects and bundles deleted");
                assert!(!sim.rcpt.contains_key(&format!("cohorts/{l}/receipt-index.signed")), "{l}: index deleted");
                assert!(!sim.rcpt.contains_key(&format!("cohorts/{l}/chunk-map.json")), "{l}: chunk map deleted");
            } else {
                assert_eq!((due, deleted), (None, None), "{l}: retained");
                assert!(rows > 0, "{l}: receipt rows retained");
            }
        }
        if k == 1 {
            // The probe build's index and map under its probe prefix went too;
            // its root and count stay on the cohort row.
            assert!(!sim.rcpt.contains_key("probes/v1/receipt-index.signed"));
            assert!(!sim.rcpt.contains_key("probes/v1/chunk-map.json"));
            assert_eq!(roots(pseq), probe_roots);
        }
    }
    // Signed packages are never deleted, and the status reports the rule.
    assert!(sim.rcpt.contains_key("cohorts/testnet-rv1-r0/package.signed"));
    let st = tx(&db, |s| engine::status(s, sim.now)).unwrap();
    assert_eq!(st["receipt_retention"]["rule"], super::retention::RULE);
    assert_eq!(st["receipt_retention"]["retained_cohorts"], 2);
    let first = st["cohorts"].as_array().unwrap().iter().find(|c| c["label"] == "testnet-rv1-r0").unwrap();
    assert_eq!(first["receipt_retention"]["retained"], false);
    assert_eq!(first["receipt_retention"]["due_after_cohort"], 4);
    assert!(first["receipt_count"].as_i64().unwrap() > 0, "the count stays");
    assert!(!tx(&db, |s| engine::has_work(s, &cfg())).unwrap(), "nothing left to delete");
}

/// Closed cohorts set up directly (reward-v0 shape: no closure record).
fn closed_cohorts(db: &Sqlite, numbers: &[u64]) -> Vec<i64> {
    numbers
        .iter()
        .map(|n| {
            let mut p = plan(10);
            p.cohort = *n;
            p.label = format!("testnet-ret-{n}");
            let bytes = serde_json::to_vec(&p).unwrap();
            let seq = tx(db, |s| engine::queue(s, &cfg(), &bytes, 1_000_000)).unwrap();
            db.conn
                .execute(
                    "UPDATE cohorts SET phase = 'closed', v3 = NULL, reserved_microusd = 1, settled_microusd = 1,
                        purged_ms = 1 WHERE seq = ?1",
                    [seq],
                )
                .unwrap();
            db.conn
                .execute(
                    "INSERT INTO receipts (seq, attempt_id, job, task, number, outcome, object_sha256, object_len, object_key)
                     VALUES (?1, 'a', 0, 0, 1, 0, 'x', 1, 'k')",
                    [seq],
                )
                .unwrap();
            seq
        })
        .collect()
}

/// New commitments to a target need its receipts: while cohort N's target is
/// the active one its receipts stay whatever completes after it, and they
/// become due once a later target activates (at the next completion). An
/// abandoned cohort is not a completion; its own receipts follow the rule.
#[test]
fn an_active_targets_receipts_stay_and_abandoned_cohorts_are_not_completions() {
    let db = Sqlite::open();
    sql::migrate(&db, &EVAL_COHORT_SCHEMA).unwrap();
    let seqs = closed_cohorts(&db, &[1, 2, 3, 4, 5]);
    db.conn.execute("INSERT INTO eval_targets (seq, target_digest, activated_epoch) VALUES (?1, 'aa', 1)", [seqs[0]]).unwrap();
    db.conn.execute("UPDATE cohorts SET phase = 'abandoned' WHERE seq = ?1", [seqs[2]]).unwrap();
    assert_eq!(tx(&db, super::retention::mark_due).unwrap(), 2);
    let due = |s: i64| retention_of(&db, s).0;
    // Completed: 2, 4, 5 (3 abandoned). Cohort 1's target is active.
    assert_eq!(due(seqs[0]), None, "the active target's cohort is retained");
    assert_eq!(due(seqs[1]), Some(5), "cohort 2: completions 4 and 5");
    assert_eq!(due(seqs[2]), Some(5), "abandoned cohort 3: completions 4 and 5");
    assert_eq!(due(seqs[3]), None);
    assert_eq!(due(seqs[4]), None);
    // A later target activates: cohort 1 is due after its second later completion.
    db.conn.execute("INSERT INTO eval_targets (seq, target_digest, activated_epoch) VALUES (?1, 'bb', 2)", [seqs[3]]).unwrap();
    assert_eq!(tx(&db, super::retention::mark_due).unwrap(), 1);
    assert_eq!(due(seqs[0]), Some(4));
    // Marking is idempotent and work is reported until deletion finishes.
    assert_eq!(tx(&db, super::retention::mark_due).unwrap(), 0);
    assert!(tx(&db, super::retention::has_work).unwrap());
    let Some(Work::RetireReceipts { seq, prefix, keys, marker }) =
        tx(&db, |s| super::retention::next_work(s, &cfg())).unwrap()
    else {
        panic!("retire")
    };
    assert_eq!(seq, seqs[0]);
    assert_eq!(prefix, "cohorts/testnet-ret-1/receipts/");
    assert_eq!(keys, vec!["cohorts/testnet-ret-1/receipt-index.signed", "cohorts/testnet-ret-1/chunk-map.json"]);
    // The content-free marker goes first, until it is recorded.
    let (key, body) = marker.expect("marker");
    assert_eq!(key, "cohorts/testnet-ret-1/receipts-retired.json");
    let m: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(m, json!({"schema": super::retention::contract::RETIRED_MARKER_SCHEMA, "label": "testnet-ret-1", "cohort": 1,
        "rule": super::retention::RULE, "due_after_cohort": 4}));
    tx(&db, |s| super::retention::note_retiring(s, seqs[0], 1)).unwrap();
    let Some(Work::RetireReceipts { marker, .. }) = tx(&db, |s| super::retention::next_work(s, &cfg())).unwrap() else {
        panic!("retire")
    };
    assert_eq!(marker, None, "recorded once");
}

/// Probe builds are not completions: they have no package or target and
/// stay outside the public record, whose N+1 and N+2 the coordinator's rule
/// must match. Their own receipts follow the rule.
#[test]
fn probe_builds_are_not_completions() {
    let db = Sqlite::open();
    sql::migrate(&db, &EVAL_COHORT_SCHEMA).unwrap();
    let seqs = closed_cohorts(&db, &[1, 2, 3]);
    db.conn.execute("UPDATE cohorts SET v3 = '{}', closure_digest = 'probe-build' WHERE seq = ?1", [seqs[1]]).unwrap();
    assert_eq!(tx(&db, super::retention::mark_due).unwrap(), 0, "one later completion (3) for cohort 1");
    let more = closed_cohorts(&db, &[4]);
    assert_eq!(tx(&db, super::retention::mark_due).unwrap(), 2);
    assert_eq!(retention_of(&db, seqs[0]).0, Some(4), "cohort 1: completions 3 and 4, not the probe build");
    assert_eq!(retention_of(&db, seqs[1]).0, Some(4), "the probe build: completions 3 and 4");
    assert_eq!(retention_of(&db, seqs[2]).0, None);
    assert_eq!(retention_of(&db, more[0]).0, None);
}

/// The targets a signing path reads are held: the newest reward-v1 target
/// (the row `v3::active_target_digest` signs for), which a later reward-v0
/// target never releases, and the newest target of any rule (the
/// predecessor a reward-v0 package chains to). A rule that stopped producing
/// targets is released once a later target activates. A released target's
/// cohort is due after the completion that released it when that is later
/// than its second later completion.
#[test]
fn the_targets_signing_reads_are_held() {
    let db = Sqlite::open();
    sql::migrate(&db, &EVAL_COHORT_SCHEMA).unwrap();
    let seqs = closed_cohorts(&db, &[1, 2, 3, 4, 5, 6]);
    let target = |seq: i64, digest: &str, rule: &str| {
        db.conn
            .execute(
                "INSERT INTO eval_targets (seq, target_digest, activated_epoch, rule) VALUES (?1, ?2, 1, ?3)",
                rusqlite::params![seq, digest, rule],
            )
            .unwrap();
    };
    target(seqs[0], "aa", "reward-v1");
    target(seqs[1], "bb", "testnet-reward-v0");
    tx(&db, super::retention::mark_due).unwrap();
    let due = |s: i64| retention_of(&db, s).0;
    assert_eq!(due(seqs[0]), None, "the reward-v1 target's cohort stays after a later v0 target");
    assert_eq!(due(seqs[1]), None, "the v0 rule's latest target stays too");
    assert_eq!(due(seqs[2]), Some(5));
    // A later v1 target (cohort 6) supersedes cohort 1's: due after 6, the
    // completion that released it, not after 3.
    target(seqs[5], "cc", "reward-v1");
    tx(&db, super::retention::mark_due).unwrap();
    assert_eq!(due(seqs[0]), Some(6));
    assert_eq!(due(seqs[1]), Some(6), "the stale v0 target is released by the later target, at cohort 6");
    let st = tx(&db, |s| engine::status(s, 0)).unwrap();
    let held: Vec<i64> = st["cohorts"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|c| c["receipt_retention"]["held"] == true)
        .map(|c| c["cohort"].as_i64().unwrap())
        .collect();
    assert_eq!(held, vec![6]);
}

/// The current authorization holds its target's cohort (not its carrier,
/// pending the user's decision on the N+2 rule), read from the private
/// pointer before a deletion step,
/// with no count or time limit: a due cohort it names goes back to waiting
/// until its deletion has started, and becomes due again once the pointer
/// names something else.
#[test]
fn the_current_authorization_holds_its_target_cohort() {
    use super::retention::{Hold, apply_hold, hold_of, pointer_ref};
    use slop_ninja_admission_wire::json::{AUTHORIZATION_POINTER_SCHEMA, Chain, ObjectRef};
    use slop_ninja_protocol::{Digest, RewardIntentAuthorization, sign};
    let db = Sqlite::open();
    sql::migrate(&db, &EVAL_COHORT_SCHEMA).unwrap();
    let seqs = closed_cohorts(&db, &[1, 2, 3, 4, 5, 6]);
    let t1 = hex::encode([0xa1; 32]);
    let m2 = hex::encode([0xb2; 32]);
    db.conn.execute("UPDATE cohorts SET manifest_digest = ?1 WHERE seq = ?2", rusqlite::params![m2, seqs[1]]).unwrap();
    for (seq, d) in [(seqs[0], t1.clone()), (seqs[3], hex::encode([0xa4; 32]))] {
        db.conn
            .execute(
                "INSERT INTO eval_targets (seq, target_digest, activated_epoch, rule) VALUES (?1, ?2, 1, 'reward-v1')",
                rusqlite::params![seq, d],
            )
            .unwrap();
    }
    // Without a hold: 1 (superseded by 4), 2 and 3 are due; 4 is active.
    tx(&db, super::retention::mark_due).unwrap();
    let due = |s: i64| retention_of(&db, s).0;
    assert_eq!((due(seqs[0]), due(seqs[1]), due(seqs[2]), due(seqs[3])), (Some(4), Some(4), Some(5), None));
    // The pointer still names an authorization for target 1, carried by
    // cohort 2: cohort 1 goes back to waiting; the carrier 2 and cohort 3
    // stay due.
    let key = slop_ninja_protocol::SigningKey::from_bytes(&[9; 32]);
    let a = sign(
        RewardIntentAuthorization {
            chain: cfg().chain,
            cohort: 2,
            cohort_manifest: Digest([0xb2; 32]),
            score_package: Digest([0xc2; 32]),
            target: Digest([0xa1; 32]),
            checkpoint_generation: 1,
            issued_epoch: 10,
            renew_by_epoch: 11,
        },
        &key,
    )
    .unwrap()
    .to_bytes()
    .unwrap();
    let sha = slop_ninja_protocol::object_sha256(&a).hex();
    let chain = Chain::from_binding(&cfg().chain);
    let pointer = json!({"schema": AUTHORIZATION_POINTER_SCHEMA, "chain": chain, "kind": "cohort",
        "authorization": {"path": format!("authorizations/{sha}.signed"), "sha256": sha, "bytes": a.len()},
        "renewal": null});
    let (kind, r) = pointer_ref(pointer.to_string().as_bytes(), &cfg().chain).unwrap();
    let hold = hold_of(&kind, &r, &a).unwrap();
    assert_eq!(hold, Hold::Cohort { cohort: 2, manifest: m2.clone(), target: t1.clone() });
    assert!(!tx(&db, |s| apply_hold(s, &hold, seqs[0], 100)).unwrap(), "cohort 1 is held");
    assert_eq!((due(seqs[0]), due(seqs[1]), due(seqs[2])), (None, Some(4), Some(5)));
    assert_eq!(tx(&db, super::retention::mark_due).unwrap(), 0, "still held");
    let Some(Work::RetireReceipts { seq, .. }) = tx(&db, |s| super::retention::next_work(s, &cfg())).unwrap() else {
        panic!("retire")
    };
    assert_eq!(seq, seqs[1], "the carrier is not held");
    tx(&db, |s| super::retention::note_retiring(s, seqs[1], 150)).unwrap();
    // Once a cohort's deletion started (its marker is stored), a hold no
    // longer returns it to waiting.
    tx(&db, |s| super::retention::note_retiring(s, seqs[2], 200)).unwrap();
    let m3 = Hold::Cohort { cohort: 3, manifest: String::new(), target: String::new() };
    db.conn.execute("UPDATE cohorts SET manifest_digest = '' WHERE seq = ?1", [seqs[2]]).unwrap();
    assert!(tx(&db, |s| apply_hold(s, &m3, seqs[2], 300)).unwrap());
    // A launch authorization (or no pointer) names no receipts: 1 is due
    // again at the next marking.
    assert!(tx(&db, |s| apply_hold(s, &Hold::Launch, seqs[2], 400)).unwrap());
    assert_eq!(tx(&db, super::retention::mark_due).unwrap(), 1);
    assert_eq!((due(seqs[0]), due(seqs[1])), (Some(4), Some(4)));
    let st = tx(&db, super::retention::status).unwrap();
    assert_eq!(st["authorization_hold"]["kind"], "launch");
    // A pointer or object that does not match is refused (nothing deleted).
    assert_eq!(pointer_ref(b"{}", &cfg().chain), Err("authorization_pointer"));
    let elsewhere = slop_ninja_protocol::ChainBinding { netuid: 602, ..cfg().chain };
    assert_eq!(pointer_ref(pointer.to_string().as_bytes(), &elsewhere), Err("authorization_chain"));
    let other = ObjectRef { path: r.path.clone(), sha256: hex::encode([0; 32]), bytes: r.bytes };
    assert_eq!(hold_of("cohort", &other, &a), Err("authorization_object"));
    assert_eq!(hold_of("renewal", &r, &a), Err("authorization_kind"));
    let moved = json!({"schema": AUTHORIZATION_POINTER_SCHEMA, "chain": chain, "kind": "cohort",
        "authorization": {"path": "authorizations/other.signed", "sha256": sha, "bytes": a.len()}});
    assert_eq!(pointer_ref(moved.to_string().as_bytes(), &cfg().chain), Err("authorization_pointer"));
}

/// A probe build's index and chunk map under its probe prefix belong to the
/// latest finished build of that version: an earlier build of the same
/// version (rebuilt) never deletes them; a later build abandoned before its
/// finish never wrote them.
#[test]
fn probe_prefix_keys_belong_to_the_latest_build_of_their_version() {
    let db = Sqlite::open();
    sql::migrate(&db, &EVAL_COHORT_SCHEMA).unwrap();
    let mut sim = Sim::new(&db, cfg(), 2_000_000);
    let weak = v3_artifact(&mut sim, super::plan::tests::identity(0, 0xb0), [1, 2, 3, 4, 5]);
    let probe_plan = |cohort: u64, label: &str| {
        let mut p = plan_v3(cohort, label, 20, weak.clone());
        p.schema = super::plan::PROBE_PLAN_SCHEMA.into();
        p.reward_v1.as_mut().unwrap().probe_version = Some(1);
        p.reward_v1.as_mut().unwrap().canary.draw_humans = 0;
        serde_json::to_vec(&p).unwrap()
    };
    let mut probes = Vec::new();
    for (n, l) in [(1u64, "testnet-probe-a"), (2, "testnet-probe-b"), (3, "testnet-probe-c")] {
        let bytes = probe_plan(n, l);
        let seq = tx(&db, |s| engine::queue(s, &cfg(), &bytes, 1_000)).unwrap();
        db.conn
            .execute(
                "UPDATE cohorts SET phase = 'closed', closure_digest = 'probe-build', reserved_microusd = 1,
                    settled_microusd = 1, purged_ms = 1 WHERE seq = ?1",
                [seq],
            )
            .unwrap();
        probes.push(seq);
    }
    // c was abandoned before its finish: it wrote nothing under the prefix.
    db.conn.execute("UPDATE cohorts SET phase = 'abandoned', closure_digest = NULL WHERE seq = ?1", [probes[2]]).unwrap();
    closed_cohorts(&db, &[4, 5, 6, 7]);
    let probe_keys = ["probes/v1/receipt-index.signed".to_owned(), "probes/v1/chunk-map.json".to_owned()];
    let mut seen = Vec::new();
    while let Some(Work::RetireReceipts { seq, keys, .. }) = tx(&db, |s| super::retention::next_work(s, &cfg())).unwrap() {
        seen.push((seq, probe_keys.iter().all(|k| keys.contains(k))));
        tx(&db, |s| super::retention::finish(s, seq, super::retention::PAGE, 5_000)).unwrap();
    }
    assert_eq!(
        seen,
        vec![(probes[0], false), (probes[1], true), (probes[2], false), (seen[3].0, false), (seen[4].0, false)],
        "a's keys were overwritten by b; b owns them; c never wrote them"
    );
}

/// Row deletion is paged and resumable: each finish deletes at most a page
/// of rows, and only the transaction that removes the last row marks the
/// cohort deleted, so a step cut short at any point continues.
#[test]
fn receipt_row_deletion_is_paged_and_marks_only_when_empty() {
    let db = Sqlite::open();
    sql::migrate(&db, &EVAL_COHORT_SCHEMA).unwrap();
    let seqs = closed_cohorts(&db, &[1, 2, 3]);
    for a in ["b", "c"] {
        db.conn
            .execute(
                "INSERT INTO receipts (seq, attempt_id, job, task, number, outcome, object_sha256, object_len, object_key)
                 VALUES (?1, ?2, 0, 0, 1, 0, 'x', 1, 'k')",
                rusqlite::params![seqs[0], a],
            )
            .unwrap();
    }
    db.conn
        .execute("INSERT INTO receipt_objects (seq, key, sha256, bytes, receipts) VALUES (?1, 'k', 'x', 1, 3)", [seqs[0]])
        .unwrap();
    tx(&db, super::retention::mark_due).unwrap();
    tx(&db, |s| super::retention::note_page(s, seqs[0], 7)).unwrap();
    let mut rounds = 0;
    while !tx(&db, |s| super::retention::finish(s, seqs[0], 1, 9_000)).unwrap() {
        rounds += 1;
        assert_eq!(retention_of(&db, seqs[0]).1, None, "not marked while rows remain");
    }
    assert_eq!(rounds, 2, "three receipt rows at one per page (the object row goes with the first two)");
    let (due, deleted, objects, rows) = retention_of(&db, seqs[0]);
    assert_eq!((due, deleted, objects, rows), (Some(3), Some(9_000), 7, 0));
    // The other cohorts' rows are untouched.
    assert_eq!(retention_of(&db, seqs[1]).3, 1);
    // Finishing again is a no-op that keeps the first mark.
    assert!(tx(&db, |s| super::retention::finish(s, seqs[0], 1, 10_000)).unwrap());
    assert_eq!(retention_of(&db, seqs[0]).1, Some(9_000));
}

/// Drives `db`'s deletion steps in `cf_eval`'s order (hold, marker, pages,
/// rows) with the authorization `hold`, until no step is due; the number of
/// cohorts whose receipts it finished. Panics on a loop.
fn retire_steps(db: &Sqlite, c: &EvalConfig, hold: &super::retention::Hold) -> usize {
    let mut finished = 0;
    for _ in 0..100 {
        let Some(Work::RetireReceipts { seq, marker, .. }) = tx(db, |s| super::retention::next_work(s, c)).unwrap() else {
            return finished;
        };
        if !tx(db, |s| super::retention::apply_hold(s, hold, seq, 3_000_000)).unwrap() {
            continue;
        }
        if marker.is_some() {
            tx(db, |s| super::retention::note_retiring(s, seq, 3_000_000)).unwrap();
        }
        if tx(db, |s| super::retention::finish(s, seq, super::retention::PAGE, 4_000_000)).unwrap() {
            finished += 1;
        }
    }
    panic!("deletion steps never end");
}

/// A superseded predecessor completes nothing more: its successor tells it
/// after the successor's second completion, and every remaining cohort of the
/// predecessor becomes due. The notice is refused before supersession and for
/// a cohort number that is not later than the predecessor's own; the
/// successor repeats it until the predecessor reports nothing left.
#[test]
fn a_superseded_predecessor_deletes_on_its_successors_notice() {
    use crate::config::{Authority, Supersede};
    // The predecessor: three closed cohorts, the last one's target active.
    let old = Sqlite::open();
    sql::migrate(&old, &EVAL_COHORT_SCHEMA).unwrap();
    let seqs = closed_cohorts(&old, &[1, 2, 3]);
    old.conn.execute("INSERT INTO eval_targets (seq, target_digest, activated_epoch) VALUES (?1, 'aa', 1)", [seqs[2]]).unwrap();
    assert_eq!(tx(&old, |s| super::retention::accept_notice(s, 20)).unwrap(), Err("not_superseded"));
    tx(&old, super::retention::mark_due).unwrap();
    let retired = predecessor_cfg();
    let req = Supersede::of(&Authority::named("r0b", 13).unwrap()).unwrap();
    // Cohort 1 is due under the predecessor's own rule (2 and 3 completed).
    // A due deletion is work for its alarm but never holds up succession:
    // it finishes under the successor's notices.
    assert!(tx(&old, |s| engine::has_work(s, &retired)).unwrap());
    assert!(!tx(&old, |s| engine::blocking_work(s, &retired)).unwrap());
    tx(&old, |s| super::succession::supersede(s, &retired, &req, 1_000_000)).unwrap().unwrap();
    assert_eq!(retention_of(&old, seqs[0]).0, Some(3));
    assert_eq!(tx(&old, |s| super::retention::accept_notice(s, 3)).unwrap(), Err("notice_cohort"));
    // The successor: two completed cohorts (13 and 14) and two predecessors.
    let new = Sqlite::open();
    sql::migrate(&new, &EVAL_COHORT_SCHEMA).unwrap();
    let succ = EvalConfig { authority: Authority::named_after("r0c", 13, &["r0b"]).unwrap(), ..cfg() };
    closed_cohorts(&new, &[13]);
    assert_eq!(tx(&new, |s| super::retention::notice_due(s, &succ)).unwrap(), None, "one completion is not enough");
    closed_cohorts(&new, &[14, 15]);
    let Some(Work::RetentionNotice { predecessor, after_cohort }) =
        tx(&new, |s| super::retention::notice_due(s, &succ)).unwrap()
    else {
        panic!("notice")
    };
    assert_eq!((predecessor.as_str(), after_cohort), ("main", 14));
    // The predecessor marks its remaining cohorts (2, and 3 whose target the
    // restart superseded) due and reports what is left.
    let v = tx(&old, |s| super::retention::accept_notice(s, after_cohort)).unwrap().unwrap();
    assert_eq!(v["remaining"], 3);
    assert_eq!(retention_of(&old, seqs[0]).0, Some(3), "already due under its own rule");
    assert_eq!(retention_of(&old, seqs[2]).0, Some(14));
    // Before the predecessor reports nothing left, the successor reports it
    // as stalled once a completion follows the notice's (15 after 14).
    let stalled = tx(&new, |s| super::retention::stalled(s, &succ)).unwrap();
    let names: Vec<&str> = stalled.iter().map(|v| v["predecessor"].as_str().unwrap()).collect();
    assert_eq!(names, vec!["main", "r0b"]);
    assert_eq!(stalled[0]["completions_since"], 1);
    // The real step order (`cf_eval`'s RetireReceipts arm): the hold first,
    // then the marker, pages and rows. The accepted notice released the
    // target the predecessor still lists, so the hold returns nothing to
    // waiting and every cohort goes.
    assert_eq!(retire_steps(&old, &retired, &super::retention::Hold::None), 3);
    assert!(!tx(&old, |s| engine::has_work(s, &retired)).unwrap(), "no alarm left armed");
    let v = tx(&old, |s| super::retention::accept_notice(s, after_cohort)).unwrap().unwrap();
    assert_eq!(v["remaining"], 0);
    tx(&new, |s| super::retention::record_notice(s, "main", after_cohort, 5_000_000)).unwrap();
    let Some(Work::RetentionNotice { predecessor, .. }) = tx(&new, |s| super::retention::notice_due(s, &succ)).unwrap()
    else {
        panic!("second notice")
    };
    assert_eq!(predecessor, "r0b");
    tx(&new, |s| super::retention::record_notice(s, "r0b", after_cohort, 5_000_000)).unwrap();
    assert_eq!(tx(&new, |s| super::retention::notice_due(s, &succ)).unwrap(), None);
    // The original authority, and a retired one, tell nobody.
    assert_eq!(tx(&new, |s| super::retention::notice_due(s, &cfg())).unwrap(), None);
    assert_eq!(tx(&new, |s| super::retention::notice_due(s, &retired)).unwrap(), None);
}

/// In a superseded predecessor, an authorization the pointer still names
/// keeps its cohort through the accepted notice; once the pointer names
/// something else, the successor's repeated notice marks it due again and it
/// goes. Before any notice, the predecessor's own last target stays held.
#[test]
fn a_predecessors_authorization_hold_ends_with_the_pointer() {
    use super::retention::Hold;
    use crate::config::{Authority, Supersede};
    let old = Sqlite::open();
    sql::migrate(&old, &EVAL_COHORT_SCHEMA).unwrap();
    let seqs = closed_cohorts(&old, &[1, 2, 3, 4]);
    old.conn.execute("UPDATE cohorts SET manifest_digest = 'm4' WHERE seq = ?1", [seqs[3]]).unwrap();
    old.conn
        .execute("INSERT INTO eval_targets (seq, target_digest, activated_epoch, rule) VALUES (?1, 't1', 1, 'reward-v1')", [seqs[0]])
        .unwrap();
    let retired = predecessor_cfg();
    let req = Supersede::of(&Authority::named("r0b", 13).unwrap()).unwrap();
    tx(&old, |s| super::succession::supersede(s, &retired, &req, 1_000_000)).unwrap().unwrap();
    // Its own rule: 2 is due (3 and 4 completed); 1's target is still held.
    assert_eq!(retire_steps(&old, &retired, &Hold::None), 1);
    assert_eq!(retention_of(&old, seqs[0]).1, None, "the predecessor's target stays before any notice");
    // The pointer names an authorization of 1's target carried by 4: the
    // target's cohort is held, the carrier is not (pending the user's
    // decision on holding carriers).
    let held = Hold::Cohort { cohort: 4, manifest: "m4".into(), target: "t1".into() };
    let v = tx(&old, |s| super::retention::accept_notice(s, 14)).unwrap().unwrap();
    assert_eq!(v["remaining"], 3);
    assert_eq!(retire_steps(&old, &retired, &held), 2, "3 and the carrier 4 go; 1 is held by the authorization");
    assert_eq!(retention_of(&old, seqs[0]).0, None);
    let v = tx(&old, |s| super::retention::accept_notice(s, 14)).unwrap().unwrap();
    assert_eq!(v["remaining"], 1);
    assert_eq!(retire_steps(&old, &retired, &Hold::Launch), 1);
    let v = tx(&old, |s| super::retention::accept_notice(s, 14)).unwrap().unwrap();
    assert_eq!(v["remaining"], 0);
    let st = tx(&old, super::retention::status).unwrap();
    assert_eq!(st["notice_accepted_after"], 14);
}

/// A due cohort whose receipts remain after a further completion is
/// reported as stalled (no clock: completions only); held cohorts are not.
#[test]
fn a_due_cohort_left_after_a_further_completion_is_stalled() {
    let db = Sqlite::open();
    sql::migrate(&db, &EVAL_COHORT_SCHEMA).unwrap();
    let seqs = closed_cohorts(&db, &[1, 2, 3]);
    assert!(tx(&db, |s| super::retention::stalled(s, &cfg())).unwrap().is_empty(), "due after 3, nothing later");
    closed_cohorts(&db, &[4]);
    let st = tx(&db, |s| super::retention::stalled(s, &cfg())).unwrap();
    assert_eq!(st.len(), 1, "cohort 1 (due after 3) after completion 4; cohort 2 is due only now");
    assert_eq!((st[0]["cohort"].as_u64(), st[0]["due_after_cohort"].as_u64()), (Some(1), Some(3)));
    // Marking changes nothing: the report covers due and newly due alike.
    tx(&db, super::retention::mark_due).unwrap();
    assert_eq!(tx(&db, |s| super::retention::stalled(s, &cfg())).unwrap().len(), 1);
    assert_eq!(retire_steps(&db, &cfg(), &super::retention::Hold::None), 2);
    assert_eq!(retention_of(&db, seqs[0]).1, Some(4_000_000));
    assert!(tx(&db, |s| super::retention::stalled(s, &cfg())).unwrap().is_empty());
}

/// While the active cohort only waits on its lanes (nothing to send), a due
/// deletion runs; root work of the active cohort still comes first.
#[test]
fn deletion_runs_while_the_active_cohort_waits_on_its_lanes() {
    let db = Sqlite::open();
    sql::migrate(&db, &EVAL_COHORT_SCHEMA).unwrap();
    let seqs = closed_cohorts(&db, &[1, 2, 3]);
    let mut p = plan(10);
    p.cohort = 4;
    p.label = "testnet-ret-4".into();
    let active = tx(&db, |s| engine::queue(s, &cfg(), &serde_json::to_vec(&p).unwrap(), 1_000_000)).unwrap();
    db.conn.execute("UPDATE cohorts SET phase = 'generating' WHERE seq = ?1", [active]).unwrap();
    // No documents yet: the draw is root work and comes first.
    assert!(matches!(tx(&db, |s| engine::next_work(s, &cfg(), 2_000_000)).unwrap(), Some(Work::Draw { .. })));
    db.conn
        .execute(
            "INSERT INTO docs (seq, idx, key, kind, domain, source_group, status) VALUES (?1, 0, 'k', 'ai_only', 'd', 'g', 'pending')",
            [active],
        )
        .unwrap();
    // Generation pending and no lane to kick (no heartbeat yet): deletion runs.
    let Some(Work::RetireReceipts { seq, .. }) = tx(&db, |s| engine::next_work(s, &cfg(), 2_000_000)).unwrap() else {
        panic!("retire while the active cohort waits")
    };
    assert_eq!(seq, seqs[0]);
    assert_eq!(retire_steps(&db, &cfg(), &super::retention::Hold::None), 1);
    assert!(matches!(
        tx(&db, |s| engine::next_work(s, &cfg(), 2_000_000)).unwrap(),
        Some(Work::Dispatch { ref kicks, .. }) if kicks.is_empty()
    ));
}

// ------------------------------------------------- stuck states (audit A1, B1, B2, C2)

/// A1/D2: a coordinator outage leaves scoring lanes silent past their
/// staleness bound. Their unreported bound is tracked and settled, but it is
/// time passing, not spend: it never trips the scoring budget, and the same
/// cohort resumes and finishes after the outage.
#[test]
fn a_coordinator_outage_never_trips_the_scoring_budget() {
    let db = Sqlite::open();
    sql::migrate(&db, &EVAL_COHORT_SCHEMA).unwrap();
    let mut p = wide(20, 1);
    let f = p.fanout.as_mut().unwrap();
    let ub = super::cost::nano_to_micro_ceil(super::cost::scoring_upper_bound_nanousd(2, 20, 6, &f.cost));
    f.score.scoring_budget_microusd = ub;
    let seq = open_cohort(&db, &p, 10, 1_000_000);
    let mut sim = Sim::new(&db, cfg(), 1_000_010);
    sim.claims_per_run = 1;
    for _ in 0..100 {
        sim.tick();
        let preds: i64 = db.conn.query_row("SELECT COUNT(*) FROM preds", [], |r| r.get(0)).unwrap();
        if preds > 0 {
            break;
        }
    }
    assert_eq!((phase(&db, seq).as_str(), fault_of(&db, seq)), ("scoring", None));
    // Three hours with no heartbeat, no step and no lane report.
    sim.now += 3 * 3_600_000;
    let st = status(&db, sim.now);
    let unreported = st["cohorts"][0]["lanes"]["spend"]["scoring_unreported_bound_microusd"].as_u64();
    assert!(unreported.unwrap_or(0) > ub, "the silent lanes' bound alone exceeds the budget: {unreported:?}");
    assert_eq!(sim.run(seq, 400), "transition", "the same cohort resumes and finishes");
    assert_eq!(fault_of(&db, seq), None, "an outage never trips the budget");
}

/// D2: platform refusals (an arena or object bound a corrected deployment
/// lifts) pause the cohort and an operator clear resumes the same cohort; they
/// never force abandonment.
#[test]
fn platform_faults_pause_and_resume_the_same_cohort() {
    for code in ["arena_capacity", "arena_defect", "object_over_bound", engine::STEP_REFUSED] {
        let db = Sqlite::open();
        sql::migrate(&db, &EVAL_COHORT_SCHEMA).unwrap();
        let p = wide(20, 0);
        let seq = open_cohort(&db, &p, 10, 1_000_000);
        tx(&db, |s| engine::fault(s, seq, code, 1_000_100)).unwrap();
        assert!(tx(&db, engine::active).unwrap().is_none(), "{code}: paused");
        let cleared = tx(&db, |s| engine::clear_fault(s, &p.label, 1_000_200)).unwrap();
        assert_eq!((cleared.cleared, cleared.refused), (true, None), "{code}");
        assert_eq!(tx(&db, engine::active).unwrap().map(|c| c.seq), Some(seq), "{code}: the same cohort resumes");
    }
}

/// B2: a root step's error is recorded against its cohort. A transient one
/// (a bucket read) is retried and shown; one no wait changes (a digest that
/// does not match its record) pauses the cohort with a resumable fault that
/// names the code once, and the operator can clear or abandon it.
#[test]
fn a_deterministic_step_error_pauses_its_cohort_resumably() {
    assert!(engine::step_error_is_deterministic("pool manifest digest"));
    assert!(engine::step_error_is_deterministic("artifact manifest missing"));
    assert!(engine::step_error_is_deterministic("EVAL_PACKAGE_SIGNING_KEY"));
    assert!(engine::step_error_is_deterministic("package signing key is not the configured trust root"));
    // Fix round 1: an object that is not there is told apart from a read
    // that failed.
    assert!(engine::step_error_is_deterministic("summary missing"));
    assert!(engine::step_error_is_deterministic("used ledger missing"));
    for transient in ["pool manifest read", "closure put", "seal commit", "arena_retry", "arena_poisoned", "next_work", "summary read", "used ledger read"] {
        assert!(!engine::step_error_is_deterministic(transient), "{transient}");
    }
    let db = Sqlite::open();
    sql::migrate(&db, &EVAL_COHORT_SCHEMA).unwrap();
    let p = wide(20, 0);
    let seq = open_cohort(&db, &p, 10, 1_000_000);
    assert!(!tx(&db, |s| engine::note_step_error(s, seq, "pool manifest read", 1_000_100)).unwrap());
    assert!(!tx(&db, |s| engine::note_step_error(s, seq, "pool manifest read", 1_000_200)).unwrap());
    let e = &status(&db, 1_000_200)["cohorts"][0]["step_error"];
    assert_eq!((e["code"].as_str(), e["count"].as_i64(), e["since_ms"].as_i64()), (Some("pool manifest read"), Some(2), Some(1_000_100)));
    assert_eq!(e["deterministic"], false);
    assert_eq!(fault_of(&db, seq), None, "a transient error never pauses");
    // Fix round 1: the stored epoch at its first failure, kept while it
    // persists across later epochs (the alert judges it by chain progress).
    let first_epoch = tx(&db, engine::observed).unwrap().epoch.unwrap().index;
    assert_eq!(e["since_epoch"].as_u64(), Some(first_epoch));
    kick_at(&db, first_epoch + 1, 1_000_250);
    assert!(!tx(&db, |s| engine::note_step_error(s, seq, "pool manifest read", 1_000_260)).unwrap());
    let e = &status(&db, 1_000_260)["cohorts"][0]["step_error"];
    assert_eq!((e["since_epoch"].as_u64(), e["count"].as_i64()), (Some(first_epoch), Some(3)));
    // Fix round 2: a step failing on alternating transient codes keeps the
    // epoch it first failed in, so the alert still sees its persistence.
    kick_at(&db, first_epoch + 2, 1_000_270);
    for (k, code) in ["summary read", "closure put", "summary read"].iter().enumerate() {
        assert!(!tx(&db, |s| engine::note_step_error(s, seq, code, 1_000_271 + k as u64)).unwrap());
    }
    let e = &status(&db, 1_000_280)["cohorts"][0]["step_error"];
    assert_eq!((e["code"].as_str(), e["since_epoch"].as_u64(), e["count"].as_i64()), (Some("summary read"), Some(first_epoch), Some(1)));
    // A row written before v18 (no epoch) takes the current stored epoch.
    db.conn.execute("UPDATE cohorts SET step_error_since_epoch = NULL WHERE seq = ?1", [seq]).unwrap();
    assert!(!tx(&db, |s| engine::note_step_error(s, seq, "closure put", 1_000_290)).unwrap());
    let e = &status(&db, 1_000_290)["cohorts"][0]["step_error"];
    assert_eq!(e["since_epoch"].as_u64(), Some(first_epoch + 2));
    // A completed step clears it.
    tx(&db, |s| engine::note_step(s, seq, 5)).unwrap();
    assert!(status(&db, 1_000_300)["cohorts"][0]["step_error"].is_null());
    // A digest mismatch pauses it, logged once with its code.
    assert!(tx(&db, |s| engine::note_step_error(s, seq, "pool manifest digest", 1_000_400)).unwrap());
    assert!(tx(&db, |s| engine::note_step_error(s, seq, "pool manifest digest", 1_000_500)).unwrap());
    assert_eq!(fault_of(&db, seq).as_deref(), Some(engine::STEP_REFUSED));
    let notes: Vec<String> = status(&db, 1_000_500)["cohorts"][0]["log"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|l| l["note"].as_str().map(str::to_owned))
        .filter(|n| n.contains("step_refused"))
        .collect();
    assert_eq!(notes, ["fault:step_refused", "step_refused:pool manifest digest"]);
    assert!(engine::RESUMABLE_FAULTS.contains(&engine::STEP_REFUSED));
    let cleared = tx(&db, |s| engine::clear_fault(s, &p.label, 1_000_600)).unwrap();
    assert!(cleared.cleared);
    assert_eq!(tx(&db, engine::active).unwrap().map(|c| c.seq), Some(seq));
    // Paused again, the operator may also end it.
    tx(&db, |s| engine::refuse_step(s, seq, "fingerprint vector digest", 1_000_700)).unwrap();
    assert!(tx(&db, |s| engine::abandon(s, &p.label, "step_refused", 1_000_800)).unwrap());
}

/// B1: a closed reward-v1 cohort whose closure cannot be written no longer
/// holds the head of every step. After a failed attempt it waits its retry
/// time while other work runs, and one that fails in a way no wait changes
/// pauses only its own cohort (resumably) and blocks nothing else.
#[test]
fn a_closure_that_cannot_be_written_holds_up_no_other_work() {
    let db = Sqlite::open();
    sql::migrate(&db, &EVAL_COHORT_SCHEMA).unwrap();
    let seqs = closed_cohorts(&db, &[30, 31]);
    let (closing, other) = (seqs[0], seqs[1]);
    db.conn.execute("UPDATE cohorts SET v3 = '{}', package_digest = 'p' WHERE seq = ?1", [closing]).unwrap();
    // The other cohort's spend is not yet settled: work behind the closure.
    db.conn.execute("UPDATE cohorts SET settled_microusd = NULL WHERE seq = ?1", [other]).unwrap();
    let next = |now: u64| tx(&db, |s| engine::next_work(s, &cfg(), now)).unwrap();
    let now = 2_000_000u64;
    assert_eq!(next(now), Some(Work::Close { seq: closing }));
    // A transient failure: the closure waits its retry time; the rest runs.
    tx(&db, |s| engine::note_step_error(s, closing, "closure put", now)).unwrap();
    assert!(matches!(next(now + 1), Some(Work::Settle { seq, .. }) if seq == other), "{:?}", next(now + 1));
    assert_eq!(next(now + engine::STEP_RETRY_MS), Some(Work::Close { seq: closing }), "retried after its wait");
    // A closure that can never be written (a key that is not deployed).
    tx(&db, |s| engine::note_step_error(s, closing, "EVAL_PACKAGE_SIGNING_KEY", now + engine::STEP_RETRY_MS)).unwrap();
    assert_eq!(fault_of(&db, closing).as_deref(), Some(engine::STEP_REFUSED));
    for later in [now + 2 * engine::STEP_RETRY_MS, now + 86_400_000] {
        assert!(matches!(next(later), Some(Work::Settle { seq, .. }) if seq == other), "nothing else waits on it");
    }
    // With the rest done, the paused closure re-arms no alarm, still counts
    // as held work for a succession, and the operator's clear runs it again
    // at once.
    db.conn.execute("UPDATE cohorts SET settled_microusd = 1 WHERE seq = ?1", [other]).unwrap();
    assert!(!tx(&db, |s| engine::has_due_work(s, &cfg(), now + 86_400_000)).unwrap());
    assert!(tx(&db, |s| engine::blocking_work(s, &cfg())).unwrap());
    assert!(tx(&db, |s| engine::clear_fault(s, "testnet-ret-30", now + 86_400_000)).unwrap().cleared);
    assert_eq!(next(now + 86_400_000), Some(Work::Close { seq: closing }));
}

/// C2: a multi-hour outage, then a fallback catch-up that walks the lagging
/// confirmed watermark across a dozen historical epoch boundaries with a
/// tick every minute. None of the replayed boundaries is credited (they
/// passed while nobody observed them), so the cohort's admission window is
/// not compressed into the catch-up; once caught up, the next live boundary
/// is credited as usual and the same cohort moves on.
#[test]
fn a_multi_hour_catch_up_replay_credits_no_historical_tempo() {
    let db = Sqlite::open();
    sql::migrate(&db, &EVAL_COHORT_SCHEMA).unwrap();
    let mut now = 1_000_000u64;
    let p = wide(20, 0); // admission 1 tempo
    let bytes = serde_json::to_vec(&p).unwrap();
    tx(&db, |s| engine::queue(s, &cfg(), &bytes, now)).unwrap();
    assert!(kick_at(&db, 10, now).is_empty());
    now += 60_000;
    assert_eq!(kick_at(&db, 11, now), ["testnet-rehearsal-1:admission"]);
    let o = tx(&db, engine::observed).unwrap();
    assert_eq!(o.active_tempos, 1);
    // Three hours of silence; the head is now in epoch 25.
    now += 3 * 3_600_000;
    let head = 25u64;
    // The catch-up: 93 heights a tick from epoch 11's block to the head's.
    let mut block = 1000 + 11 * 361;
    let end = 1000 + head * 361;
    let mut ticks = 0;
    while block < end {
        block = (block + 93).min(end);
        now += 60_000;
        ticks += 1;
        let index = (block - 1000) / 361;
        let moved = kick_lagging(&db, block, index, head, now);
        assert!(moved.is_empty(), "tick {ticks} at epoch {index}: {moved:?}");
    }
    assert!(ticks > 40, "a multi-hour catch-up: {ticks} ticks");
    let o = tx(&db, engine::observed).unwrap();
    assert_eq!(o.epoch.unwrap().index, head);
    assert_eq!((o.active_tempos, o.uncredited_tempos), (1, head - 11), "no replayed boundary is credited");
    assert_eq!(phase(&db, 1), "admission", "the admission window is not compressed into the catch-up");
    // Caught up: the head's epoch is observed live; the next boundary credits.
    now += 60_000;
    assert!(kick_at(&db, head, now).is_empty());
    now += 60_000;
    assert_eq!(kick_at(&db, head + 1, now), ["testnet-rehearsal-1:generating"]);
    let o = tx(&db, engine::observed).unwrap();
    assert_eq!(o.active_tempos, 2);
}

/// C2: a watermark a few blocks behind the head at a boundary tick does not
/// lose the boundary's credit: an epoch observed live once stays live.
#[test]
fn a_boundary_tick_behind_the_head_keeps_its_credit() {
    let db = Sqlite::open();
    sql::migrate(&db, &EVAL_COHORT_SCHEMA).unwrap();
    let now = 1_000_000u64;
    kick_at(&db, 10, now);
    // The boundary to 11 has passed at the head, the watermark is still in 10.
    kick_lagging(&db, 1000 + 10 * 361 + 300, 10, 11, now + 60_000);
    kick_at(&db, 11, now + 120_000);
    let o = tx(&db, engine::observed).unwrap();
    assert_eq!((o.active_tempos, o.uncredited_tempos), (1, 0));
}

/// Fix round 1 (C2): a boundary tick whose head reading failed or was
/// refused by the RPC budget proves no lag. The new epoch is not recorded,
/// so the next tick that reads it live credits the boundary; a proven lag
/// (a head epoch ahead of the watermark's) still settles it as uncredited.
#[test]
fn a_boundary_tick_without_a_head_reading_defers_the_increment() {
    let db = Sqlite::open();
    sql::migrate(&db, &EVAL_COHORT_SCHEMA).unwrap();
    let now = 1_000_000u64;
    kick_at(&db, 10, now);
    let unknown = |index: u64, block: u64, now: u64| {
        let at = BlockRef { number: block, hash: Id([(block % 251) as u8; 32]) };
        let k = Kick { confirmed: Some(at), epoch: Some(EpochObservation { block: at, index, last_epoch_block: 990 + index * 361 }), head_epoch: None };
        tx(&db, |s| engine::observe(s, &k, now)).unwrap();
    };
    // Two ticks in epoch 11 without a head reading: nothing is settled.
    unknown(11, 1000 + 11 * 361, now + 60_000);
    unknown(11, 1000 + 11 * 361 + 4, now + 120_000);
    let o = tx(&db, engine::observed).unwrap();
    assert_eq!(o.epoch.unwrap().index, 10, "the new epoch waits for a head reading");
    assert_eq!((o.active_tempos, o.uncredited_tempos, o.kicks), (0, 0, 3));
    assert_eq!(o.confirmed.unwrap().number, 1000 + 11 * 361 + 4, "the watermark still moves");
    // Fix round 2: the watermark's epoch is reported beside the stored one,
    // so a deferral that persists shows (the publisher's epoch_deferred).
    let st = status(&db, now + 120_000);
    assert_eq!((st["watermark_epoch"].as_u64(), st["observed"]["epoch"]["index"].as_u64()), (Some(11), Some(10)));
    // The next tick reads epoch 11 live: the boundary is credited.
    kick_at(&db, 11, now + 180_000);
    let o = tx(&db, engine::observed).unwrap();
    assert_eq!((o.epoch.unwrap().index, o.active_tempos, o.uncredited_tempos), (11, 1, 0));
    assert_eq!(status(&db, now + 180_000)["watermark_epoch"].as_u64(), Some(11));
    // Within a stored epoch an unknown head changes nothing.
    unknown(11, 1000 + 11 * 361 + 30, now + 240_000);
    assert!(tx(&db, engine::observed).unwrap().epoch_live, "observed live once, it stays live");
    // A proven lag still settles: the head is already in epoch 13.
    kick_lagging(&db, 1000 + 12 * 361 + 2, 12, 13, now + 300_000);
    let o = tx(&db, engine::observed).unwrap();
    assert_eq!((o.epoch.unwrap().index, o.active_tempos, o.uncredited_tempos), (12, 1, 1));
}

/// Fix round 1 (C2): the tick's head-epoch decision. A watermark at the
/// head reuses its own reading, a head ahead needs one read there, and a
/// missing, older or conflicting head is unknown (never a lag).
#[test]
fn the_tick_reads_the_head_epoch_only_when_the_head_is_ahead() {
    use engine::{HeadEpoch, head_epoch_source};
    let at = |n: u64, h: u8| BlockRef { number: n, hash: Id([h; 32]) };
    let confirmed = at(5_000, 1);
    assert_eq!(head_epoch_source(confirmed, 13, Some(confirmed)), HeadEpoch::Known(13));
    assert_eq!(head_epoch_source(confirmed, 13, Some(at(5_002, 2))), HeadEpoch::ReadAt(at(5_002, 2)));
    assert_eq!(head_epoch_source(confirmed, 13, None), HeadEpoch::Unknown);
    assert_eq!(head_epoch_source(confirmed, 13, Some(at(5_000, 9))), HeadEpoch::Unknown, "conflicting hashes");
    assert_eq!(head_epoch_source(confirmed, 13, Some(at(4_990, 3))), HeadEpoch::Unknown, "a head behind the watermark");
}

/// Follow-ups verify item 8: a predecessor notice that came back with work
/// left is retried once a tick on an otherwise idle store, instead of
/// waiting for the next cohort's steps (and paging `retention_stalled`
/// meanwhile). A delivery kick (no epoch) does not arm it, and nothing does
/// once the predecessor reported nothing left.
#[test]
fn a_due_predecessor_notice_is_retried_each_tick_on_an_idle_store() {
    use crate::config::Authority;
    let db = Sqlite::open();
    sql::migrate(&db, &EVAL_COHORT_SCHEMA).unwrap();
    let succ = EvalConfig { authority: Authority::named_after("r0c", 13, &["r0b"]).unwrap(), ..cfg() };
    closed_cohorts(&db, &[13, 14]);
    assert!(tx(&db, |s| super::retention::notice_due(s, &succ)).unwrap().is_some());
    let now = 5_000_000;
    assert!(!tx(&db, |s| engine::has_due_work(s, &succ, now)).unwrap(), "idle otherwise: the alarm never re-arms for it");
    let tick = Kick { epoch: Some(EpochObservation { block: BlockRef { number: 9_000, hash: Id([9; 32]) }, index: 30, last_epoch_block: 8_990 }), ..Kick::default() };
    assert!(tx(&db, |s| engine::kick_due(s, &succ, &tick, now)).unwrap(), "a tick arms one notice attempt");
    assert!(!tx(&db, |s| engine::kick_due(s, &succ, &Kick::default(), now)).unwrap(), "a delivery does not");
    tx(&db, |s| super::retention::record_notice(s, "main", 14, now)).unwrap();
    tx(&db, |s| super::retention::record_notice(s, "r0b", 14, now)).unwrap();
    assert!(!tx(&db, |s| engine::kick_due(s, &succ, &tick, now)).unwrap(), "nothing left to tell");
}
