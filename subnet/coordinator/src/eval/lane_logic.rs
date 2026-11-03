//! The scoring lane's decisions, shared by the Workers lane (`cf_lanes.rs`)
//! and the native simulation through [`LaneIo`]: reading a claim's texts from
//! sealed shards, turning scorer replies into outcomes, running one claimed
//! chunk to its commit or hand-back ([`run_chunk`]), and crash isolation
//! ([`isolate`]).
//!
//! **Crash isolation** (testnet decision of 2026-10-30, made resumable and
//! reproduction-based after the review of 2026-10-31): a request that killed
//! its container gets infrastructure-error receipts, and the chunk's next claim
//! runs each unresolved document alone. A document that kills its instance is
//! run again alone on a freshly restarted instance that has just scored a fixed
//! synthetic control document, and is a model error for that document of that
//! job only if it kills that instance too (utility 0 there); the job and the
//! cohort proceed. A failed request whose container stays up is an
//! infrastructure error. Each outcome is recorded with the cohort as it
//! resolves, and a run that reaches its invocation's step budget hands the
//! attempt back, so the next claim continues it under the same attempt number.

use crate::eval::engine::{self, CallTiming};
use crate::eval::lanes::{ArtifactRef, Assignment, ChunkClaim, ChunkCommit, IsoReply, LaneRef, Meter};
use crate::eval::receipts::{self, ChunkFacts, TaskOutcome};
use crate::eval::score;
use serde_json::{Value, json};
use sha2::{Digest as _, Sha256};
use std::collections::BTreeMap;

/// The control document: fixed synthetic text, never a task.
pub const CONTROL_TEXT: &str = "The committee met on Tuesday to review the budget for the new library wing. \
Members asked about heating costs and the timeline for the roof repairs.";
pub const CONTROL_TASK: u32 = u32::MAX;

pub fn sha_hex(b: &[u8]) -> String {
    hex::encode(Sha256::digest(b))
}

/// A claim's (task, text, digest) triples.
pub type Docs = Vec<(u32, String, String)>;

/// The texts of one sealed shard by task, after checking the shard's digest and
/// each text's digest.
pub fn shard_texts(body: &[u8], shard_sha256: &str) -> Result<BTreeMap<u32, (String, String)>, &'static str> {
    if sha_hex(body) != shard_sha256 {
        return Err("batch shard digest");
    }
    let mut out = BTreeMap::new();
    for line in std::str::from_utf8(body).map_err(|_| "batch shard utf-8")?.lines() {
        let v: Value = serde_json::from_str(line).map_err(|_| "batch shard line")?;
        let (Some(task), Some(text), Some(sha)) = (v["task"].as_u64(), v["text"].as_str(), v["text_sha256"].as_str()) else {
            return Err("batch shard fields");
        };
        if sha_hex(text.as_bytes()) != sha {
            return Err("batch shard text digest");
        }
        out.insert(task as u32, (text.to_owned(), sha.to_owned()));
    }
    Ok(out)
}

/// The claim's documents, each text matching the digest the claim names.
pub fn claim_docs(claim: &ChunkClaim, texts: &BTreeMap<u32, (String, String)>) -> Result<Docs, &'static str> {
    claim
        .tasks
        .iter()
        .zip(&claim.digests)
        .map(|(t, d)| match texts.get(t) {
            Some((text, sha)) if sha == d => Ok((*t, text.clone(), sha.clone())),
            Some(_) => Err("claimed text digest differs from the shard"),
            None => Err("claimed task missing from its shards"),
        })
        .collect()
}

/// Outcomes for every task from a scorer reply; a task the reply does not
/// answer (or answers with another text digest) is an infrastructure error.
pub fn outcomes_from_results(docs: &[(u32, String, String)], results: &[Value]) -> Vec<TaskOutcome> {
    let by_id: BTreeMap<&str, &Value> = results.iter().filter_map(|r| Some((r["id"].as_str()?, r))).collect();
    docs.iter()
        .map(|(task, _, sha)| match by_id.get(task.to_string().as_str()) {
            Some(v) => {
                let (o, h, a, t) = score::outcome(v, sha);
                TaskOutcome::new(*task, &o, h, a, t)
            }
            None => TaskOutcome::infrastructure(*task),
        })
        .collect()
}

/// Why a scorer request failed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ScoreFailure {
    /// Provably never reached the model: the instance was not running, or the
    /// scorer refused the request before inference (409, 503).
    NotDispatched,
    /// Dispatched, and the container stayed up and answering: an
    /// infrastructure error, never evidence against the artifact.
    Failed,
    /// Dispatched, and the container died: it stopped, stopped answering, or
    /// the request outlasted its bound and the lane destroyed it.
    Died,
}

/// Why a restart (destroy, start, stream and load) failed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RestartFailure {
    /// The platform: no start, no answer, an R2 read, the build identity.
    Infra(&'static str),
    /// The artifact, on a freshly started instance: its manifest or parts, a
    /// load that failed or outlasted its allowance, or death during the load.
    /// `definitive` for a deterministic check (the size bound, the manifest
    /// digest, the scorer's admission refusal), which attributes at once.
    Artifact { code: &'static str, definitive: bool },
}

/// A cohort call that did not answer `200`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CohortError {
    /// The cohort refused the call (422): it did not take effect.
    Refused,
    /// No answer after the retries: it may or may not have taken effect.
    Unreachable,
}

/// Points at which a load-mode fault (or, natively, a simulated lane death)
/// may end a lane's invocation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Fault {
    /// The alarm handler throws right after a claim (the platform retries it).
    ThrowAfterClaim,
    /// The invocation dies after its request, before any put.
    DieBeforePut,
    /// The invocation dies after both puts, before its commit.
    DieAfterPut,
}

/// Everything a scoring lane does outside this module: object storage, its
/// cohort object, its container and its invocation's clock. The Workers lane
/// (`cf_lanes.rs`) and the native simulation implement it, so both run
/// [`run_chunk`] and [`isolate`] unchanged.
#[allow(async_fn_in_trait)]
pub trait LaneIo {
    /// `Ok(None)` is a confirmed absence; `Err` a read that failed.
    async fn eval_get(&mut self, key: &str) -> Result<Option<Vec<u8>>, ()>;
    async fn eval_put(&mut self, key: &str, body: Vec<u8>) -> Result<(), ()>;
    async fn rcpt_get(&mut self, key: &str) -> Result<Option<Vec<u8>>, ()>;
    async fn rcpt_put(&mut self, key: &str, body: Vec<u8>) -> Result<(), ()>;
    /// One call to the lane's cohort object.
    async fn cohort(&mut self, path: &str, body: Value) -> Result<Value, CohortError>;
    /// One scorer request on the current instance, bounded by `timeout_ms`.
    async fn score(&mut self, docs: &[(u32, String, String)], timeout_ms: u64) -> Result<(Vec<Value>, CallTiming), ScoreFailure>;
    /// Destroys the instance and starts a fresh one with the artifact loaded.
    async fn restart(&mut self) -> Result<(), RestartFailure>;
    /// The bound on one restart: the start wait plus the stream and load allowances.
    fn restart_bound_ms(&self) -> u64;
    /// Whether a step bounded by `ms` may start now: it ends within the
    /// invocation's step budget, and no newer kick has superseded the lane.
    async fn may_run(&mut self, ms: u64) -> bool;
    fn now_ms(&self) -> u64;
    async fn sleep(&mut self, ms: u64);
    /// Whether `f` ends this invocation now (load-mode faults; native tests).
    async fn fault(&mut self, f: Fault) -> bool;
    /// The lane's cumulative metering, sent with each commit.
    async fn meter(&mut self) -> Meter;
}

/// One claim's context: who the lane is and what it scores.
pub struct ChunkCtx<'a> {
    pub r: &'a LaneRef,
    pub name: &'a str,
    pub a: &'a Assignment,
    pub art: &'a ArtifactRef,
    pub seed: &'a [u8; 32],
    pub prefix: &'a str,
}

/// How a claimed chunk ended for the lane loop.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChunkEnd {
    /// Claim the next chunk.
    Next,
    /// Re-arm after this many ms (the claim went back).
    Rearm(u64),
    /// The invocation ends here (a load-mode fault, or a simulated death).
    Died,
    /// The alarm handler throws (load-mode fault).
    Throw,
}

/// Batch shards already read in this invocation, by key.
pub type ShardCache = BTreeMap<String, BTreeMap<u32, (String, String)>>;

/// Backoff after a claim went back on a failure.
const FAILURE_REARM_MS: u64 = 5_000;
/// First put retry delay; it doubles while the claim's lease has time left.
const PUT_RETRY_MS: u64 = 1_000;
/// Margin kept between the last put retry and the claim's lease expiry.
const LEASE_MARGIN_MS: u64 = 5_000;

fn claim_body(cx: &ChunkCtx<'_>, c: &ChunkClaim) -> Value {
    json!({"lane": cx.r, "name": cx.name, "job": c.job, "chunk": c.chunk, "claim": c.claim, "attempt": c.attempt})
}

async fn call<I: LaneIo>(io: &mut I, path: &str, cx: &ChunkCtx<'_>, c: &ChunkClaim, extra: Value) -> Result<Value, CohortError> {
    let mut body = claim_body(cx, c);
    if let (Some(b), Some(x)) = (body.as_object_mut(), extra.as_object()) {
        for (k, v) in x {
            b.insert(k.clone(), v.clone());
        }
    }
    io.cohort(path, body).await
}

/// Puts with retries while the claim's lease leaves time for them.
async fn put_retry<I: LaneIo>(io: &mut I, receipts: bool, key: &str, body: &[u8], deadline: u64) -> Result<(), ()> {
    let mut wait = PUT_RETRY_MS;
    loop {
        let r = if receipts { io.rcpt_put(key, body.to_vec()).await } else { io.eval_put(key, body.to_vec()).await };
        if r.is_ok() {
            return Ok(());
        }
        if io.now_ms() + wait > deadline {
            return Err(());
        }
        io.sleep(wait).await;
        wait *= 2;
    }
}

/// The claim's documents, read from the sealed shards (cached per invocation)
/// and checked by digest. `Err` when a shard read fails or does not match:
/// nothing was dispatched.
async fn read_docs<I: LaneIo>(io: &mut I, cx: &ChunkCtx<'_>, c: &ChunkClaim, cache: &mut ShardCache) -> Result<Docs, &'static str> {
    for (key, sha) in &c.shards {
        if cache.contains_key(key) {
            continue;
        }
        if cx.a.faults.r2_outage_until_ms > io.now_ms() {
            // Load-mode fault: an R2 read outage.
            return Err("batch shard read");
        }
        let body = match io.eval_get(key).await {
            Ok(Some(b)) => b,
            _ => return Err("batch shard read"),
        };
        cache.insert(key.clone(), shard_texts(&body, sha)?);
    }
    let mut texts = BTreeMap::new();
    for key in c.shards.iter().map(|s| &s.0) {
        if let Some(t) = cache.get(key) {
            texts.extend(t.iter().map(|(k, v)| (*k, v.clone())));
        }
    }
    claim_docs(c, &texts)
}

/// Runs one claimed chunk to its commit or hand-back.
///
/// - **Before dispatch** (no cohort manifest, a shard read or digest failure, a
///   refused request) the claim goes back with no attempt spent
///   (`/lane/give-back`), counting a scorer failure, so an outage pauses the
///   cohort (`scorer_unavailable`, resumable) instead of spending attempts.
/// - **Reconciliation** of an expired attempt commits its durable results
///   object with no compute (re-storing a missing receipt object, which is a
///   pure function of it); a read error goes back for a later retry, and only a
///   confirmed absence records the attempt lost.
/// - **After dispatch**, results first, then the receipt object, then the
///   commit; puts retry while the lease has time, then the claim is released
///   with its attempt allocated, for reconciliation.
pub async fn run_chunk<I: LaneIo>(io: &mut I, cx: &ChunkCtx<'_>, c: &ChunkClaim, cache: &mut ShardCache) -> ChunkEnd {
    let deadline = io.now_ms() + c.lease_ms.saturating_sub(LEASE_MARGIN_MS);
    let give_back = json!({"failure": true});
    let (Some(manifest), Some(artifact)) = (cx.a.manifest.as_deref().and_then(engine::parse_digest), engine::parse_digest(&cx.art.id)) else {
        let _ = call(io, "/lane/give-back", cx, c, give_back).await;
        return ChunkEnd::Rearm(FAILURE_REARM_MS);
    };
    let facts = ChunkFacts {
        manifest,
        artifact,
        seed: cx.seed,
        prefix: cx.prefix,
        job: c.job,
        chunk: c.chunk,
        attempt: c.attempt,
        lost: &c.lost,
        record_only: c.record_only,
        tasks: &c.tasks,
    };
    let rkey = results_key(&cx.a.label, c.job, c.chunk, c.attempt);
    if c.reconcile {
        return reconcile(io, cx, c, &facts, &rkey, deadline).await;
    }
    if c.record_only {
        let Ok(built) = receipts::build_chunk(&facts, &[]) else { return ChunkEnd::Next };
        if put_retry(io, true, &built.object_key, &built.object, deadline).await.is_err() {
            let _ = call(io, "/lane/release", cx, c, give_back).await;
            return ChunkEnd::Rearm(FAILURE_REARM_MS);
        }
        commit(io, cx, c, Vec::new(), built, CallTiming::default(), false, false).await;
        return ChunkEnd::Next;
    }
    if io.fault(Fault::ThrowAfterClaim).await {
        return ChunkEnd::Throw;
    }
    let done: BTreeMap<u32, TaskOutcome> = c.done.iter().map(|o| (o.task, o.clone())).collect();
    let (outcomes, crashed, timing) = if c.model_error {
        // The job's artifact failed: no container, every other task a model error.
        (merge(c, &done, true), false, CallTiming::default())
    } else {
        let docs = match read_docs(io, cx, c, cache).await {
            Ok(d) => d,
            Err(_) => {
                let _ = call(io, "/lane/give-back", cx, c, give_back).await;
                return ChunkEnd::Rearm(FAILURE_REARM_MS);
            }
        };
        if c.isolate {
            let iso = isolate(io, cx, c, &docs, &done).await;
            let done: BTreeMap<u32, TaskOutcome> = done.into_iter().chain(iso.new.iter().map(|o| (o.task, o.clone()))).collect();
            match iso.end {
                IsoEnd::Complete => (merge(c, &done, false), false, iso.timing),
                IsoEnd::ArtifactFailed => (merge(c, &done, true), false, iso.timing),
                IsoEnd::Fenced => return ChunkEnd::Next,
                IsoEnd::Yield { failure } => {
                    let _ = call(io, "/lane/iso-yield", cx, c, json!({"failure": failure})).await;
                    return if failure { ChunkEnd::Rearm(FAILURE_REARM_MS) } else { ChunkEnd::Rearm(0) };
                }
            }
        } else {
            match io.score(&docs, c.lease_ms.saturating_sub(15_000).max(5_000)).await {
                Ok((results, t)) => (outcomes_from_results(&docs, &results), false, t),
                Err(ScoreFailure::NotDispatched) => {
                    let _ = call(io, "/lane/give-back", cx, c, give_back).await;
                    return ChunkEnd::Rearm(FAILURE_REARM_MS);
                }
                // Dispatched, container up: infrastructure errors, retried normally.
                Err(ScoreFailure::Failed) => (c.tasks.iter().map(|t| TaskOutcome::infrastructure(*t)).collect(), false, CallTiming::default()),
                // The container died: the next claim isolates.
                Err(ScoreFailure::Died) => (c.tasks.iter().map(|t| TaskOutcome::infrastructure(*t)).collect(), true, CallTiming::default()),
            }
        }
    };
    if io.fault(Fault::DieBeforePut).await {
        return ChunkEnd::Died;
    }
    let deadline = if c.isolate && !c.model_error {
        // An isolation run renewed its lease as it went: the puts get a full one from here.
        if call(io, "/lane/renew", cx, c, json!({})).await != Ok(Value::Bool(true)) {
            return ChunkEnd::Next;
        }
        io.now_ms() + c.lease_ms.saturating_sub(LEASE_MARGIN_MS)
    } else {
        deadline
    };
    let Ok(built) = receipts::build_chunk(&facts, &outcomes) else {
        let _ = call(io, "/lane/release", cx, c, give_back).await;
        return ChunkEnd::Rearm(FAILURE_REARM_MS);
    };
    // Results first, then the receipt object, then the commit.
    let results_body = serde_json::to_vec(&outcomes).unwrap_or_default();
    if put_retry(io, false, &rkey, &results_body, deadline).await.is_err()
        || put_retry(io, true, &built.object_key, &built.object, deadline).await.is_err()
    {
        let _ = call(io, "/lane/release", cx, c, give_back).await;
        return ChunkEnd::Rearm(FAILURE_REARM_MS);
    }
    if io.fault(Fault::DieAfterPut).await {
        return ChunkEnd::Died;
    }
    commit(io, cx, c, outcomes, built, timing, crashed, false).await;
    ChunkEnd::Next
}

/// Every claimed task's outcome in task order: a recorded one, else a model
/// error when the artifact failed (`artifact`), else an infrastructure error.
fn merge(c: &ChunkClaim, done: &BTreeMap<u32, TaskOutcome>, artifact: bool) -> Vec<TaskOutcome> {
    c.tasks
        .iter()
        .map(|t| match done.get(t) {
            Some(o) => o.clone(),
            None if artifact => TaskOutcome::artifact_failed(*t),
            None => TaskOutcome::infrastructure(*t),
        })
        .collect()
}

async fn reconcile<I: LaneIo>(io: &mut I, cx: &ChunkCtx<'_>, c: &ChunkClaim, facts: &ChunkFacts<'_>, rkey: &str, deadline: u64) -> ChunkEnd {
    let retry_later = json!({"failure": true});
    let res = match io.eval_get(rkey).await {
        Err(()) => {
            // Not a confirmed absence: never record a durable answer as lost.
            let _ = call(io, "/lane/give-back", cx, c, retry_later).await;
            return ChunkEnd::Rearm(FAILURE_REARM_MS);
        }
        Ok(None) => {
            let _ = call(io, "/lane/lost", cx, c, json!({})).await;
            return ChunkEnd::Next;
        }
        Ok(Some(b)) => b,
    };
    let rebuilt = serde_json::from_slice::<Vec<TaskOutcome>>(&res).ok().and_then(|outs| Some((receipts::build_chunk(facts, &outs).ok()?, outs)));
    let Some((built, outs)) = rebuilt else {
        let _ = call(io, "/lane/lost", cx, c, json!({})).await;
        return ChunkEnd::Next;
    };
    match io.rcpt_get(&built.object_key).await {
        Err(()) => {
            let _ = call(io, "/lane/give-back", cx, c, retry_later).await;
            return ChunkEnd::Rearm(FAILURE_REARM_MS);
        }
        Ok(Some(stored)) if stored == built.object => {}
        Ok(Some(_)) => {
            let _ = call(io, "/lane/lost", cx, c, json!({})).await;
            return ChunkEnd::Next;
        }
        Ok(None) => {
            // The results object is durable; its receipt object is a pure function of it.
            if put_retry(io, true, &built.object_key, &built.object, deadline).await.is_err() {
                let _ = call(io, "/lane/give-back", cx, c, retry_later).await;
                return ChunkEnd::Rearm(FAILURE_REARM_MS);
            }
        }
    }
    commit(io, cx, c, outs, built, CallTiming::default(), false, true).await;
    ChunkEnd::Next
}

#[allow(clippy::too_many_arguments)]
async fn commit<I: LaneIo>(
    io: &mut I,
    cx: &ChunkCtx<'_>,
    c: &ChunkClaim,
    outcomes: Vec<TaskOutcome>,
    built: receipts::ChunkReceipts,
    timing: CallTiming,
    crashed: bool,
    reconciled: bool,
) {
    let cm = ChunkCommit {
        job: c.job,
        chunk: c.chunk,
        attempt: c.attempt,
        claim: c.claim,
        record_only: c.record_only,
        outcomes,
        object_key: built.object_key.clone(),
        object_sha256: sha_hex(&built.object),
        object_len: built.object.len() as u64,
        timing,
        crashed,
        reconciled,
        meter: io.meter().await,
    };
    let body = json!({"lane": cx.r, "name": cx.name, "commit": cm});
    // A lost reply is retried by the next reconciliation (an exact repeat
    // answers `already`); a refused commit goes back counted, so a commit that
    // can never land pauses the cohort instead of looping.
    if io.cohort("/lane/commit", body).await == Err(CohortError::Refused) {
        let _ = call(io, "/lane/release", cx, c, json!({"failure": true})).await;
    }
}

// ------------------------------------------------------------- crash isolation

/// How an isolation run ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IsoEnd {
    /// Every document has an outcome: commit the attempt.
    Complete,
    /// The job's artifact failed: the rest are model errors; commit.
    ArtifactFailed,
    /// Handed back unfinished (step budget, a pause, a restart or a refusal);
    /// `failure` counts a scorer failure. The next claim continues it.
    Yield { failure: bool },
    /// The claim is no longer this lane's.
    Fenced,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Isolation {
    /// Outcomes resolved by this run (each recorded with the cohort as it resolved).
    pub new: Vec<TaskOutcome>,
    pub end: IsoEnd,
    pub calls: u32,
    pub restarts: u32,
    /// Documents attributed: they killed a freshly restarted instance alone,
    /// right after it scored the control.
    pub reproduced: u32,
    pub timing: CallTiming,
}

fn control_ok(results: &[Value]) -> bool {
    let id = CONTROL_TASK.to_string();
    results.iter().any(|r| r["id"].as_str() == Some(id.as_str()) && r["status"] == "ok")
}

/// Restarts the instance under the held claim. An artifact failure is reported
/// to the cohort: if it records the job's artifact as failed, the run ends
/// `ArtifactFailed`; otherwise it yields (an entrant's failure was counted as
/// the artifact's, a reference's as a scorer failure).
async fn restart<I: LaneIo>(io: &mut I, cx: &ChunkCtx<'_>, c: &ChunkClaim, out: &mut Isolation) -> Result<(), IsoEnd> {
    let _ = call(io, "/lane/renew", cx, c, json!({})).await;
    out.restarts += 1;
    let r = io.restart().await;
    let _ = call(io, "/lane/renew", cx, c, json!({})).await;
    match r {
        Ok(()) => Ok(()),
        Err(RestartFailure::Infra(_)) => Err(IsoEnd::Yield { failure: true }),
        Err(RestartFailure::Artifact { code, definitive }) => Err(artifact_failed(io, cx, c.job, code, definitive).await),
    }
}

/// Reports an attributable artifact failure; `ArtifactFailed` when the cohort
/// now records the job's artifact as failed.
async fn artifact_failed<I: LaneIo>(io: &mut I, cx: &ChunkCtx<'_>, job: i64, code: &str, definitive: bool) -> IsoEnd {
    let body = json!({"lane": cx.r, "job": job, "code": code, "definitive": definitive});
    match io.cohort("/lane/artifact-failure", body).await {
        Ok(Value::Bool(true)) => IsoEnd::ArtifactFailed,
        _ => IsoEnd::Yield { failure: false },
    }
}

/// The isolation run over a chunk's unresolved documents, after a request
/// killed its container (testnet decision of 2026-10-30, review of 2026-10-31):
///
/// 1. It starts on a freshly restarted instance and runs each document not yet
///    resolved in this attempt alone, recording each outcome with the cohort
///    as soon as it is known (`/lane/iso-progress`, which also renews the lease).
/// 2. A request that fails with the container still up is an infrastructure
///    error for that document; a refused request, or a restart that fails,
///    hands the attempt back (counted as a scorer failure).
/// 3. A document that kills its instance is not attributed yet. A fresh
///    instance is started and must first score the control document; then the
///    document runs alone again. Only if this second run kills the proven
///    instance too is it a model error (`isolated_crash`), for that job's
///    document only. If the second run answers, its answer is the outcome.
/// 4. A control that a fresh instance cannot score is an artifact failure:
///    counted toward the job's `artifact_failures_before_model_error`, after
///    which the job's unresolved documents are model errors (entrants only).
/// 5. Before every step the lane checks that the step fits its invocation's
///    budget and that no newer kick superseded it; otherwise it hands the
///    attempt back with everything recorded so far, uncounted, and the next
///    claim continues the same attempt. No attempt number is spent on that.
pub async fn isolate<I: LaneIo>(io: &mut I, cx: &ChunkCtx<'_>, c: &ChunkClaim, docs: &[(u32, String, String)], done: &BTreeMap<u32, TaskOutcome>) -> Isolation {
    let mut out = Isolation { new: Vec::new(), end: IsoEnd::Complete, calls: 0, restarts: 0, reproduced: 0, timing: CallTiming::default() };
    let doc_ms = cx.a.doc_timeout_ms.max(1_000);
    let control = vec![(CONTROL_TASK, CONTROL_TEXT.to_owned(), sha_hex(CONTROL_TEXT.as_bytes()))];
    let mut fresh_needed = true;
    let stop = |mut out: Isolation, end: IsoEnd| {
        out.end = end;
        out
    };
    for doc in docs.iter().filter(|d| !done.contains_key(&d.0)) {
        if fresh_needed {
            if !io.may_run(io.restart_bound_ms()).await {
                return stop(out, IsoEnd::Yield { failure: false });
            }
            if let Err(end) = restart(io, cx, c, &mut out).await {
                return stop(out, end);
            }
            fresh_needed = false;
        }
        if !io.may_run(doc_ms).await {
            return stop(out, IsoEnd::Yield { failure: false });
        }
        out.calls += 1;
        let one = std::slice::from_ref(doc);
        let outcome = match io.score(one, doc_ms).await {
            Ok((results, t)) => {
                add(&mut out.timing, t);
                outcomes_from_results(one, &results).remove(0)
            }
            Err(ScoreFailure::NotDispatched) => return stop(out, IsoEnd::Yield { failure: true }),
            Err(ScoreFailure::Failed) => TaskOutcome::infrastructure(doc.0),
            Err(ScoreFailure::Died) => {
                // Prove a fresh instance on the control, then run the document alone again.
                if !io.may_run(io.restart_bound_ms() + 2 * doc_ms).await {
                    return stop(out, IsoEnd::Yield { failure: false });
                }
                if let Err(end) = restart(io, cx, c, &mut out).await {
                    return stop(out, end);
                }
                out.calls += 1;
                match io.score(&control, doc_ms).await {
                    Ok((r, _)) if control_ok(&r) => {}
                    Ok(_) | Err(ScoreFailure::Died) => {
                        // A freshly started instance of this artifact cannot score benign text.
                        let end = artifact_failed(io, cx, c.job, "control document failed on a fresh instance", false).await;
                        return stop(out, end);
                    }
                    Err(_) => return stop(out, IsoEnd::Yield { failure: true }),
                }
                out.calls += 1;
                match io.score(one, doc_ms).await {
                    Ok((results, t)) => {
                        add(&mut out.timing, t);
                        outcomes_from_results(one, &results).remove(0)
                    }
                    Err(ScoreFailure::Died) => {
                        fresh_needed = true;
                        out.reproduced += 1;
                        TaskOutcome::isolated_crash(doc.0)
                    }
                    Err(ScoreFailure::Failed) => TaskOutcome::infrastructure(doc.0),
                    Err(ScoreFailure::NotDispatched) => return stop(out, IsoEnd::Yield { failure: true }),
                }
            }
        };
        let reply = call(io, "/lane/iso-progress", cx, c, json!({"outcomes": [outcome]})).await;
        match reply.map(|v| serde_json::from_value::<IsoReply>(v).unwrap_or_default()) {
            Ok(r) if !r.held => return stop(out, IsoEnd::Fenced),
            Ok(r) => {
                out.new.push(outcome);
                if r.artifact_failed {
                    return stop(out, IsoEnd::ArtifactFailed);
                }
                if r.paused {
                    return stop(out, IsoEnd::Yield { failure: false });
                }
            }
            // Not known to be recorded: hand back without it, so no other outcome
            // of this document can be stored under this attempt by another lane.
            Err(_) => return stop(out, IsoEnd::Yield { failure: true }),
        }
    }
    out
}

fn add(t: &mut CallTiming, x: CallTiming) {
    t.call_ms += x.call_ms;
    t.server_ms += x.server_ms;
    t.cpu_ms += x.cpu_ms;
}

/// Runs a future that never waits on anything external to completion (native
/// tests drive the isolation run with synchronous scorers).
pub fn block_on<F: std::future::Future>(f: F) -> F::Output {
    let mut f = std::pin::pin!(f);
    let mut cx = std::task::Context::from_waker(std::task::Waker::noop());
    loop {
        if let std::task::Poll::Ready(v) = f.as_mut().poll(&mut cx) {
            return v;
        }
    }
}

/// The private results object of a chunk attempt (eval bucket), from which a
/// reconciliation rebuilds the receipt object.
pub fn results_key(label: &str, job: i64, chunk: i64, attempt: u8) -> String {
    format!("cohorts/{label}/results/job-{job:03}/chunk-{chunk:04}-attempt-{attempt}.json")
}

pub fn summary_key(label: &str, job: i64) -> String {
    format!("cohorts/{label}/summaries/job-{job:03}.json")
}

pub fn bundle_key(label: &str, job: i64) -> String {
    format!("{}/receipts/bundle-job-{job:03}.bin", crate::eval::engine::receipt_prefix(label))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::collections::BTreeSet;

    /// A container and a cohort: `poison` tasks kill the instance on every
    /// request holding them, `flaky` ones on their first request only, `fail`
    /// ones fail with the container up; `budget` steps fit the invocation.
    #[derive(Default)]
    struct Fake {
        poison: BTreeSet<u32>,
        flaky: BTreeSet<u32>,
        fail: BTreeSet<u32>,
        control_dies: bool,
        restarts_fail: bool,
        budget: Option<usize>,
        artifact_flagged: bool,
        calls: Vec<Vec<u32>>,
        restarts: u32,
        progress: Vec<TaskOutcome>,
        posts: Vec<String>,
    }

    impl LaneIo for Fake {
        async fn eval_get(&mut self, _: &str) -> Result<Option<Vec<u8>>, ()> {
            Ok(None)
        }
        async fn eval_put(&mut self, _: &str, _: Vec<u8>) -> Result<(), ()> {
            Ok(())
        }
        async fn rcpt_get(&mut self, _: &str) -> Result<Option<Vec<u8>>, ()> {
            Ok(None)
        }
        async fn rcpt_put(&mut self, _: &str, _: Vec<u8>) -> Result<(), ()> {
            Ok(())
        }
        async fn cohort(&mut self, path: &str, body: Value) -> Result<Value, CohortError> {
            self.posts.push(path.to_owned());
            match path {
                "/lane/iso-progress" => {
                    let o: Vec<TaskOutcome> = serde_json::from_value(body["outcomes"].clone()).unwrap();
                    self.progress.extend(o);
                    Ok(json!({"held": true, "paused": false, "artifact_failed": false}))
                }
                "/lane/artifact-failure" => Ok(json!(self.artifact_flagged)),
                _ => Ok(json!(true)),
            }
        }
        async fn score(&mut self, docs: &[(u32, String, String)], _: u64) -> Result<(Vec<Value>, CallTiming), ScoreFailure> {
            let ids: Vec<u32> = docs.iter().map(|d| d.0).collect();
            self.calls.push(ids.clone());
            if (self.control_dies && ids[0] == CONTROL_TASK) || ids.iter().any(|t| self.poison.contains(t) || self.flaky.remove(t)) {
                return Err(ScoreFailure::Died);
            }
            if ids.iter().any(|t| self.fail.contains(t)) {
                return Err(ScoreFailure::Failed);
            }
            Ok((docs
                .iter()
                .map(|(t, _, sha)| json!({"id": t.to_string(), "status": "ok", "text_sha256": sha,
                    "prediction": {"ai_confidence_percent": 50.0, "ai_share_percent": 10.0, "attribution": {}}}))
                .collect(), CallTiming::default()))
        }
        async fn restart(&mut self) -> Result<(), RestartFailure> {
            self.restarts += 1;
            if self.restarts_fail { Err(RestartFailure::Infra("no answer")) } else { Ok(()) }
        }
        fn restart_bound_ms(&self) -> u64 {
            100_000
        }
        async fn may_run(&mut self, _: u64) -> bool {
            match &mut self.budget {
                None => true,
                Some(0) => false,
                Some(n) => {
                    *n -= 1;
                    true
                }
            }
        }
        fn now_ms(&self) -> u64 {
            0
        }
        async fn sleep(&mut self, _: u64) {}
        async fn fault(&mut self, _: Fault) -> bool {
            false
        }
        async fn meter(&mut self) -> Meter {
            Meter::default()
        }
    }

    fn docs(n: u32) -> Docs {
        (0..n).map(|t| {
            let text = format!("document {t}");
            let sha = sha_hex(text.as_bytes());
            (t, text, sha)
        }).collect()
    }

    fn claim(tasks: Vec<u32>, digests: Vec<String>) -> ChunkClaim {
        ChunkClaim {
            job: 1, chunk: 0, attempt: 2, lost: vec![], record_only: false, claim: 1, reconcile: false,
            isolate: true, lease_ms: 240_000, tasks, digests, shards: vec![], done: vec![], model_error: false,
        }
    }

    fn run(f: &mut Fake, n: u32, done: &[TaskOutcome]) -> Isolation {
        let d = docs(n);
        let c = claim(d.iter().map(|x| x.0).collect(), d.iter().map(|x| x.2.clone()).collect());
        let a = Assignment { label: "l".into(), doc_timeout_ms: 60_000, ..Default::default() };
        let art = ArtifactRef { id: String::new(), package_sha256: String::new(), manifest_key: String::new() };
        let r = LaneRef { seq: 1, kind: "score".into(), lane: 0, generation: 1 };
        let cx = ChunkCtx { r: &r, name: "lane", a: &a, art: &art, seed: &[0; 32], prefix: "p" };
        let done: BTreeMap<u32, TaskOutcome> = done.iter().map(|o| (o.task, o.clone())).collect();
        block_on(isolate(f, &cx, &c, &d, &done))
    }

    fn classes(v: &[TaskOutcome]) -> Vec<(u32, u8, bool)> {
        v.iter().map(|o| (o.task, o.class, o.isolated_crash)).collect()
    }

    #[test]
    fn a_document_is_attributed_only_when_it_kills_a_proven_fresh_instance_again() {
        let mut f = Fake { poison: [2].into(), ..Default::default() };
        let r = run(&mut f, 4, &[]);
        assert_eq!(r.end, IsoEnd::Complete);
        assert_eq!(classes(&r.new), [(0, 1, false), (1, 1, false), (2, 2, true), (3, 1, false)]);
        assert_eq!((r.reproduced, r.restarts), (1, 3));
        // Fresh start; task 2 dies; a fresh instance scores the control, then
        // task 2 alone kills it again; a fresh instance for task 3.
        assert_eq!(f.calls, [vec![0], vec![1], vec![2], vec![CONTROL_TASK], vec![2], vec![3]]);
        assert_eq!(f.progress, r.new, "every outcome was recorded as it resolved");
    }

    #[test]
    fn a_one_off_failure_resolves_from_the_second_run_and_is_never_attributed() {
        // Review 2026-10-31: one transient failure used to be a model error.
        let mut f = Fake { flaky: [1].into(), ..Default::default() };
        let r = run(&mut f, 3, &[]);
        assert_eq!(classes(&r.new), [(0, 1, false), (1, 1, false), (2, 1, false)]);
        assert_eq!(r.reproduced, 0);
        // A failure with the container up is an infrastructure error, with no restart.
        let mut f = Fake { fail: [0].into(), ..Default::default() };
        let r = run(&mut f, 2, &[]);
        assert_eq!(classes(&r.new), [(0, 5, false), (1, 1, false)]);
        assert_eq!(f.restarts, 1, "only the fresh start");
    }

    #[test]
    fn a_control_a_fresh_instance_cannot_score_is_the_artifacts_failure_not_the_documents() {
        let mut f = Fake { poison: [1].into(), control_dies: true, ..Default::default() };
        let r = run(&mut f, 3, &[]);
        assert_eq!(r.end, IsoEnd::Yield { failure: false }, "counted against the artifact, not as a scorer failure");
        assert_eq!(classes(&r.new), [(0, 1, false)], "task 1 is not attributed and not recorded");
        assert!(f.posts.contains(&"/lane/artifact-failure".to_owned()));
        // Once the cohort records the artifact as failed, the run ends so the
        // rest can be committed as model errors.
        let mut f = Fake { poison: [1].into(), control_dies: true, artifact_flagged: true, ..Default::default() };
        assert_eq!(run(&mut f, 3, &[]).end, IsoEnd::ArtifactFailed);
        // A restart that fails hands back counted, with nothing dispatched.
        let mut f = Fake { restarts_fail: true, ..Default::default() };
        let r = run(&mut f, 2, &[]);
        assert_eq!(r.end, IsoEnd::Yield { failure: true });
        assert!(f.calls.is_empty() && r.new.is_empty());
    }

    #[test]
    fn the_step_budget_hands_the_attempt_back_and_a_continuation_skips_what_it_recorded() {
        // Review 2026-10-31: an isolation run could outlast the 15-minute alarm wall.
        let mut f = Fake { poison: [0, 1, 2, 3, 4, 5].into(), budget: Some(5), ..Default::default() };
        let r = run(&mut f, 6, &[]);
        assert_eq!(r.end, IsoEnd::Yield { failure: false });
        // Restart, task 0, proof, control, rerun: one document attributed, then the budget.
        assert_eq!(classes(&r.new), [(0, 2, true)]);
        assert_eq!(f.progress, r.new);
        // The next claim continues with what was recorded and never re-runs it.
        let mut g = Fake { poison: [0, 1, 2, 3, 4, 5].into(), ..Default::default() };
        let r2 = run(&mut g, 6, &r.new);
        assert_eq!(r2.end, IsoEnd::Complete);
        assert_eq!(r2.new.iter().map(|o| o.task).collect::<Vec<_>>(), [1, 2, 3, 4, 5]);
        assert!(g.calls.iter().all(|c| c != &vec![0]));
        // A continuation with every document recorded needs no instance at all.
        let all: Vec<TaskOutcome> = (0..2).map(TaskOutcome::infrastructure).collect();
        let mut h = Fake::default();
        assert_eq!(run(&mut h, 2, &all).end, IsoEnd::Complete);
        assert_eq!((h.restarts, h.calls.len()), (0, 0));
    }

    #[test]
    fn shards_and_claims_are_checked_by_digest() {
        let lines: String = docs(3)
            .iter()
            .map(|(t, text, sha)| format!("{}\n", json!({"task": t, "text": text, "text_sha256": sha})))
            .collect();
        let sha = sha_hex(lines.as_bytes());
        let texts = shard_texts(lines.as_bytes(), &sha).unwrap();
        assert_eq!(texts.len(), 3);
        assert!(shard_texts(lines.as_bytes(), &"0".repeat(64)).is_err());
        let d = docs(3);
        assert_eq!(claim_docs(&claim(vec![1, 2], vec![d[1].2.clone(), d[2].2.clone()]), &texts).unwrap().len(), 2);
        assert!(claim_docs(&claim(vec![1], vec![d[0].2.clone()]), &texts).is_err());
        assert!(claim_docs(&claim(vec![7], vec![d[0].2.clone()]), &texts).is_err());
        // A reply missing a task leaves it an infrastructure error.
        let o = outcomes_from_results(&d, &[json!({"id": "0", "status": "error", "text_sha256": d[0].2})]);
        assert_eq!(o.iter().map(|x| x.class).collect::<Vec<_>>(), [2, 5, 5]);
    }
}
