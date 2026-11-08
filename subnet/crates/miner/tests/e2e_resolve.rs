//! `resolve` against the mock coordinator, its presigned-PUT R2 stub
//! (`tests/support/mock.rs`), a local status API stand-in
//! (`tests/support/api.rs`) and a chain whose commitment log is the mock's:
//! one test per intake failure state the client resolves, or refuses to.
//! Nothing is sent to Cloudflare or a chain.

mod support;

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use anyhow::{Result, bail};
use slop_ninja_miner::client::{RequestSigner, Transfer, base_url, http_client, ss58};
use slop_ninja_miner::intake::{Action, ChainFacts, Policy, State};
use slop_ninja_miner::object::ObjectHeader;
use slop_ninja_miner::opening::{Store, Submitted};
use slop_ninja_miner::resolve::{self, ApiSource, ChainAccess, Env, RunEnd, RunOptions};
use slop_ninja_miner::status_api::Which;
use slop_ninja_miner::wire::ErrorCode;
use slop_ninja_protocol::submission::SUBMISSION_PAYLOAD_LEN;
use slop_ninja_protocol::{AccountId, TESTNET_GENESIS};
use support::api::{FakeApi, status_doc};
use support::common::*;
use support::mock::{Fault, Match, Mock};

/// The mock's commitment log as the chain: finalized facts and submission.
struct MockChain {
    mock: Arc<Mock>,
    hotkey: AccountId,
    /// Fail this many fact reads (RPC lag or outage).
    facts_fail: AtomicU32,
    /// Fail this many submissions without landing (a call that never lands).
    submit_fail: AtomicU32,
    submits: AtomicU32,
}

impl MockChain {
    fn new(mock: Arc<Mock>, hotkey: AccountId) -> Self {
        Self {
            mock,
            hotkey,
            facts_fail: AtomicU32::new(0),
            submit_fail: AtomicU32::new(0),
            submits: AtomicU32::new(0),
        }
    }
}

impl ChainAccess for MockChain {
    async fn facts(&self, hotkey: &AccountId) -> Result<ChainFacts> {
        if self
            .facts_fail
            .try_update(Ordering::SeqCst, Ordering::SeqCst, |n| n.checked_sub(1))
            .is_ok()
        {
            bail!("the RPC endpoint lags the finalized head");
        }
        Ok(self.mock.with(|s| ChainFacts {
            genesis: Some(TESTNET_GENESIS),
            finalized_block: s.finalized_block,
            registration: s.registrations.get(hotkey).copied(),
            commitment: s
                .commitments
                .get(hotkey)
                .and_then(|rows| rows.last())
                .map(|r| (r.block, r.payload.clone())),
        }))
    }

    async fn submit(&self, payload: &[u8; SUBMISSION_PAYLOAD_LEN]) -> Result<Submitted> {
        self.submits.fetch_add(1, Ordering::SeqCst);
        if self
            .submit_fail
            .try_update(Ordering::SeqCst, Ordering::SeqCst, |n| n.checked_sub(1))
            .is_ok()
        {
            bail!("the extrinsic was not included before the deadline");
        }
        let block = self.mock.with(|s| s.finalized_block + 1);
        self.mock.commit(self.hotkey, block, 0, payload);
        Ok(Submitted {
            finalized_commitment_block: Some(block),
            ..Default::default()
        })
    }
}

struct Rig {
    _dir: tempfile::TempDir,
    mock: Arc<Mock>,
    api: FakeApi,
    store: Store,
    signer: Arc<dyn RequestSigner>,
    hotkey: AccountId,
    chain: MockChain,
    artifact: (PathBuf, ObjectHeader),
}

impl Rig {
    async fn new(register: bool) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(&dir.path().join("state")).unwrap();
        let mock = Arc::new(Mock::start(config(), 2_000).await);
        let api = FakeApi::start().await;
        let artifact = packed(dir.path(), 0x21, "{}");
        let signer = dev_signer("//Alice");
        if register {
            mock.register(identity(&signer, 12));
        }
        let hotkey = signer.hotkey();
        let chain = MockChain::new(mock.clone(), hotkey);
        Self {
            _dir: dir,
            mock,
            api,
            store,
            signer,
            hotkey,
            chain,
            artifact,
        }
    }

    fn env(&self) -> Env<'_, MockChain> {
        Env {
            http: http_client(Duration::from_secs(30)).unwrap(),
            api: Some(ApiSource {
                origin: base_url(&self.api.url()).unwrap(),
                network: "testnet".into(),
                mechanism: "a".into(),
                which: Which::Current,
            }),
            coordinator: Some(base_url(&self.mock.url()).unwrap()),
            signer: Some(self.signer.clone()),
            put_policy: stub_policy(&self.mock),
            transfer: Transfer::default(),
            chain: Some(&self.chain),
            store: &self.store,
            hotkey: self.hotkey,
            artifact: Some(self.artifact.clone()),
            upload: quick(),
            now: || 1_793_846_400,
        }
    }

    fn row(&self, status: &str) -> serde_json::Value {
        serde_json::json!([{"uid": 12, "hotkey": ss58(&self.hotkey), "registered": true,
            "cohort_status": status, "reason_class": null}])
    }

    /// The feed's answer for the mock cohort, with this hotkey's row.
    fn feed(&self, status: &str) {
        let (cohort, block) = self.mock.with(|s| (s.config.cohort, s.finalized_block));
        self.api.answer(
            200,
            status_doc(cohort, "admission_open", block, Some(self.row(status))),
        );
    }
}

fn opts(watch: bool) -> RunOptions {
    RunOptions {
        policy: Policy {
            allow_replace: false,
            commit_margin_blocks: 50,
            reads_chain: true,
        },
        act: true,
        watch,
        refresh: Duration::from_millis(20),
        max_backoff: Duration::from_millis(80),
        max_wait: Some(Duration::from_secs(20)),
    }
}

async fn run(rig: &Rig, opts: &RunOptions) -> (RunEnd, Vec<(State, Action)>) {
    let mut seen = Vec::new();
    let end = resolve::run(&rig.env(), opts, |d, _| {
        seen.push((d.state, d.action.clone()))
    })
    .await
    .unwrap();
    (end, seen)
}

fn sessions(mock: &Mock) -> Vec<String> {
    mock.with(|s| {
        s.commitments
            .values()
            .flat_map(|rows| rows.iter().map(|r| hex::encode(&r.payload)))
            .collect()
    })
}

fn log_lines(store: &Store) -> Vec<serde_json::Value> {
    let path = store.root().join(resolve::LOG_FILE);
    use std::os::unix::fs::PermissionsExt as _;
    assert_eq!(
        std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
        0o600
    );
    std::fs::read_to_string(path)
        .unwrap()
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect()
}

/// A registered hotkey with nothing committed: resolve commits for the
/// admitting cohort, uploads straight to R2 and follows verification; a
/// second run takes no action (idempotent).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn not_entered_is_committed_uploaded_and_verified_once() {
    let rig = Rig::new(true).await;
    rig.feed("none");
    let (end, seen) = run(&rig, &opts(true)).await;
    assert_eq!(end.last.state, State::Verified, "{seen:?}");
    assert!(end.failed.is_none() && !end.manual);
    assert!(matches!(
        seen[0],
        (
            State::NotEntered,
            Action::Commit {
                cohort: 7,
                replace: false
            }
        )
    ));
    assert!(
        seen.iter()
            .any(|(s, a)| *s == State::Committed && matches!(a, Action::Upload { .. }))
    );
    assert_eq!(rig.chain.submits.load(Ordering::SeqCst), 1);
    assert_eq!(sessions(&rig.mock).len(), 1);
    let puts = rig.mock.puts();
    assert!(puts >= 2, "header and files");
    // The feed catches up; a rerun does nothing more.
    rig.feed("verified");
    let (end, seen) = run(&rig, &opts(true)).await;
    assert_eq!(end.last.state, State::Verified);
    assert_eq!(seen.len(), 1);
    assert_eq!(rig.chain.submits.load(Ordering::SeqCst), 1);
    assert_eq!(rig.mock.puts(), puts);
    let log = log_lines(&rig.store);
    assert!(log.iter().all(|l| l["schema"] == resolve::LOG_SCHEMA));
    assert!(
        log.iter()
            .any(|l| l["action"]["kind"] == "commit" && l["outcome"] == "done")
    );
    assert!(
        log.iter()
            .any(|l| l["action"]["kind"] == "upload" && l["outcome"] == "done")
    );
    // No secret reaches the log: no opening (its nonce), no signed URL.
    let text = std::fs::read_to_string(rig.store.root().join(resolve::LOG_FILE)).unwrap();
    assert!(!text.contains("X-Amz-Signature"));
    for (_, file) in rig.store.all_openings().unwrap() {
        assert!(!text.contains(&file.opening));
    }
}

/// An interrupted upload resumes with fresh URLs, and R2 faults (an expired
/// URL, a dropped connection) are retried per object.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_interrupted_upload_resumes_with_fresh_urls() {
    let rig = Rig::new(true).await;
    rig.feed("none");
    // One pass commits; the next starts the upload and stops after one object.
    let mut env_opts = opts(false);
    let (end, _) = run(&rig, &env_opts).await;
    assert_eq!(end.last.state, State::NotEntered);
    let mut env = rig.env();
    env.upload.stop_after_objects = Some(1);
    let seen = resolve::observe(&env).await.unwrap();
    let d = resolve::diagnose(&env, &seen, &env_opts.policy);
    assert_eq!(d.state, State::Committed);
    let outcome = resolve::act(&env, &seen, &d, &env_opts.policy).await;
    assert!(
        matches!(outcome, resolve::Outcome::Transient(_)),
        "{outcome:?}"
    );
    // The feed shows the session uploading; faults on the next PUTs.
    rig.feed("uploading");
    rig.mock.with(|s| {
        s.r2.faults = vec![(Match::File, Fault::Expire), (Match::Any, Fault::Drop)];
    });
    env_opts.watch = true;
    let (end, seen) = run(&rig, &env_opts).await;
    assert_eq!(end.last.state, State::Verified, "{seen:?}");
    assert_eq!(seen[0].0, State::Uploading);
    assert_eq!(rig.chain.submits.load(Ordering::SeqCst), 1);
    assert!(
        rig.mock.with(|s| s.counters.minted) > 3,
        "fresh URLs after the faults"
    );
}

/// A commitment that never landed (the call failed) is made again, from
/// the chain's facts, never from the feed's lag alone.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_commitment_that_did_not_land_is_made_again() {
    let rig = Rig::new(true).await;
    rig.feed("none");
    rig.chain.submit_fail.store(1, Ordering::SeqCst);
    let (end, seen) = run(&rig, &opts(true)).await;
    assert_eq!(end.last.state, State::Verified, "{seen:?}");
    assert_eq!(rig.chain.submits.load(Ordering::SeqCst), 2);
    // The second commit superseded the kept, unconfirmed opening.
    assert!(
        seen.iter()
            .any(|(_, a)| matches!(a, Action::Commit { replace: true, .. }))
    );
    assert_eq!(
        sessions(&rig.mock).len(),
        1,
        "only the landed commitment is on chain"
    );
}

/// After the cutoff nothing is committed, and the miner is told to enter
/// the next cohort.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn never_commits_after_the_cutoff() {
    let rig = Rig::new(true).await;
    rig.mock.close();
    let block = rig.mock.with(|s| s.finalized_block);
    rig.api.answer(
        200,
        status_doc(7, "admission_closing", block, Some(rig.row("none"))),
    );
    let (end, _) = run(&rig, &opts(true)).await;
    assert_eq!(end.last.state, State::MissedCutoff);
    assert!(end.last.state.settled());
    assert_eq!(rig.chain.submits.load(Ordering::SeqCst), 0);
    assert!(end.last.next.contains("next"), "{}", end.last.next);
}

/// Within the commitment's landing bound of the cutoff's earliest block:
/// no commitment, and the warning says why.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn too_close_to_the_cutoff_is_refused_with_a_warning() {
    let rig = Rig::new(true).await;
    rig.feed("none");
    // The mock's cutoff is one tempo after its open block 1,900.
    rig.mock.with(|s| s.finalized_block = 1_900 + 360 - 10);
    let (end, _) = run(&rig, &opts(true)).await;
    assert_eq!(end.last.state, State::NotEntered);
    assert!(end.manual);
    assert!(
        end.last
            .blocked
            .as_deref()
            .unwrap()
            .contains("never risks a commitment")
    );
    assert!(
        end.last.warnings.iter().any(|w| w.starts_with("late:")),
        "{:?}",
        end.last.warnings
    );
    assert_eq!(rig.chain.submits.load(Ordering::SeqCst), 0);
}

/// An unregistered hotkey: the client never registers (that burns funds).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn not_registered_is_a_manual_step() {
    let rig = Rig::new(false).await;
    rig.api.answer(
        200,
        status_doc(7, "admission_open", 2_000, Some(serde_json::json!([]))),
    );
    let (end, _) = run(&rig, &opts(true)).await;
    assert_eq!(end.last.state, State::NotRegistered);
    assert!(end.manual);
    assert_eq!(rig.chain.submits.load(Ordering::SeqCst), 0);
    assert_eq!(rig.mock.puts(), 0);
}

/// Transient trouble: the status API down (503), the coordinator paused,
/// an edge error page and RPC lag are waited out with backoff.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn outages_and_rpc_lag_are_waited_out() {
    let rig = Rig::new(true).await;
    // The status API stays at its default 503 upstream_unavailable.
    rig.chain.facts_fail.store(2, Ordering::SeqCst);
    rig.mock.with(|s| {
        s.edge_errors = 2;
    });
    let (end, seen) = run(&rig, &opts(true)).await;
    assert_eq!(end.last.state, State::Verified, "{seen:?}");
    assert!(end.last.warnings.iter().any(|w| w.contains("status API")));
    // The first passes could not commit: the chain was not read.
    assert!(
        matches!(seen[0], (State::NotEntered, Action::None)),
        "{seen:?}"
    );
    assert_eq!(rig.chain.submits.load(Ordering::SeqCst), 1);
    // A paused coordinator: the client waits, and resumes once it answers.
    let rig = Rig::new(true).await;
    rig.feed("none");
    rig.mock.with(|s| s.paused_requests = 2);
    let (end, seen) = run(&rig, &opts(true)).await;
    assert_eq!(end.last.state, State::Verified, "{seen:?}");
    assert!(seen.iter().any(|(s, _)| *s == State::Paused));
}

/// A recorded object lost on the coordinator's side is sent again (the
/// Phase D repair path), from the local object.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_lost_object_is_repaired() {
    let rig = Rig::new(true).await;
    rig.feed("none");
    let (end, _) = run(&rig, &opts(true)).await;
    assert_eq!(end.last.state, State::Verified);
    let session = {
        let env = rig.env();
        let seen = resolve::observe(&env).await.unwrap();
        seen.view.unwrap().sessions[0].session.clone()
    };
    assert!(rig.mock.lose_objects(&session, |_| true) > 0);
    let (end, seen) = run(&rig, &opts(true)).await;
    assert!(
        seen.iter()
            .any(|(s, a)| *s == State::RepairNeeded && matches!(a, Action::Repair { .. })),
        "{seen:?}"
    );
    assert_eq!(end.last.state, State::Verified);
    let env = rig.env();
    let seen = resolve::observe(&env).await.unwrap();
    assert!(seen.view.unwrap().sessions[0].repair_needed.is_empty());
}

/// A commitment for another artifact than --artifact: the client never
/// changes the submitted model by itself; with --allow-replace it commits
/// the named artifact and uploads it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn changing_the_model_needs_allow_replace() {
    let rig = Rig::new(true).await;
    rig.feed("committed");
    // Committed for artifact A (no upload yet); resolve is given artifact B.
    let (_, a) = packed(rig._dir.path(), 0x22, "{}");
    entrant(&rig.mock, &rig.store, "//Alice", 12, &a, 2_010);
    let (end, _) = run(&rig, &opts(true)).await;
    assert_eq!(end.last.state, State::OtherArtifact);
    assert!(end.manual);
    assert!(end.last.blocked.is_some());
    assert_eq!(rig.chain.submits.load(Ordering::SeqCst), 0);
    assert_eq!(rig.mock.puts(), 0);
    let mut allowed = opts(true);
    allowed.policy.allow_replace = true;
    let (end, seen) = run(&rig, &allowed).await;
    assert_eq!(end.last.state, State::Verified, "{seen:?}");
    assert_eq!(rig.chain.submits.load(Ordering::SeqCst), 1);
    assert_eq!(sessions(&rig.mock).len(), 2, "A, then B replacing it");
}

/// Only one run acts on a state directory at a time.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn one_run_acts_on_a_state_directory_at_a_time() {
    let rig = Rig::new(true).await;
    let held = resolve::lock(&rig.store).unwrap();
    let error = resolve::run(&rig.env(), &opts(false), |_, _| {})
        .await
        .unwrap_err()
        .to_string();
    assert!(error.contains("another resolve"), "{error}");
    drop(held);
    // A read-only pass needs no lock.
    let mut read_only = opts(false);
    read_only.act = false;
    let _held = resolve::lock(&rig.store).unwrap();
    resolve::run(&rig.env(), &read_only, |_, _| {})
        .await
        .unwrap();
}

/// The session id in the coordinator's view of the rig's hotkey (the
/// last listed; the tests call it while there is one).
async fn newest_session(rig: &Rig) -> String {
    let env = rig.env();
    let seen = resolve::observe(&env).await.unwrap();
    seen.view.unwrap().sessions.last().unwrap().session.clone()
}

/// A commitment-class refusal (decided by the coordinator, for example at
/// the freeze): one `--watch` run commits again for the same artifact and
/// then uploads the fresh commitment until it is verified. The refused
/// session stays in the view, never superseded; it no longer decides once
/// the fresh commitment counts, so the run neither commits twice nor loops.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_commitment_refusal_is_committed_again_then_uploaded() {
    let rig = Rig::new(true).await;
    rig.feed("none");
    let (end, _) = run(&rig, &opts(true)).await;
    assert_eq!(end.last.state, State::Verified);
    let old = newest_session(&rig).await;
    rig.mock.refuse(
        &old,
        ErrorCode::CommitmentMismatch,
        "the commitment changed",
    );
    rig.feed("refused");
    let (end, seen) = run(&rig, &opts(true)).await;
    assert_eq!(end.last.state, State::Verified, "{seen:?}");
    assert!(end.failed.is_none() && !end.manual);
    assert!(
        matches!(
            seen[0],
            (
                State::RefusedCommitment,
                Action::Commit {
                    cohort: 7,
                    replace: true
                }
            )
        ),
        "{seen:?}"
    );
    assert!(
        seen.iter()
            .any(|(s, a)| *s == State::Committed && matches!(a, Action::Upload { .. })),
        "{seen:?}"
    );
    assert!(seen.len() <= 4, "bounded passes: {seen:?}");
    assert_eq!(rig.chain.submits.load(Ordering::SeqCst), 2);
    assert_eq!(
        sessions(&rig.mock).len(),
        2,
        "the first and the fresh commitment"
    );
    assert_eq!(rig.mock.session_state(&old).as_deref(), Some("refused"));
    let view = resolve::observe(&rig.env()).await.unwrap().view.unwrap();
    assert_eq!(view.sessions.len(), 2);
    assert!(
        view.sessions
            .iter()
            .any(|s| s.session != old && s.state == "verified"),
        "{view:?}"
    );
    // The feed still says refused; a rerun does nothing more.
    let (end, seen) = run(&rig, &opts(true)).await;
    assert_eq!(end.last.state, State::Verified);
    assert_eq!(seen.len(), 1, "{seen:?}");
    assert_eq!(rig.chain.submits.load(Ordering::SeqCst), 2);
    let log = log_lines(&rig.store);
    assert_eq!(
        log.iter()
            .filter(|l| l["action"]["kind"] == "commit" && l["outcome"] == "done")
            .count(),
        2
    );
}

/// An artifact-class refusal: the refused model is never committed again;
/// a changed model needs --allow-replace, then is committed, uploaded and
/// verified in one run.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_changed_model_after_an_artifact_refusal_is_committed_and_uploaded() {
    let rig = Rig::new(true).await;
    rig.feed("none");
    let (end, _) = run(&rig, &opts(true)).await;
    assert_eq!(end.last.state, State::Verified);
    let old = newest_session(&rig).await;
    rig.mock
        .refuse(&old, ErrorCode::ArtifactInvalid, "the model does not load");
    rig.feed("refused");
    // The refused model again: a manual step.
    let (end, _) = run(&rig, &opts(true)).await;
    assert_eq!(end.last.state, State::RefusedArtifact);
    assert!(end.manual);
    assert_eq!(rig.chain.submits.load(Ordering::SeqCst), 1);
    // A changed model, without and then with --allow-replace.
    let changed = packed(rig._dir.path(), 0x23, "{}");
    let mut env = rig.env();
    env.artifact = Some(changed);
    let end = resolve::run(&env, &opts(true), |_, _| {}).await.unwrap();
    assert_eq!(end.last.state, State::RefusedArtifact);
    assert!(end.manual);
    assert!(
        end.last
            .blocked
            .as_deref()
            .unwrap()
            .contains("--allow-replace"),
        "{:?}",
        end.last.blocked
    );
    assert_eq!(rig.chain.submits.load(Ordering::SeqCst), 1);
    let mut allowed = opts(true);
    allowed.policy.allow_replace = true;
    let mut seen = Vec::new();
    let end = resolve::run(&env, &allowed, |d, _| {
        seen.push((d.state, d.action.clone()))
    })
    .await
    .unwrap();
    assert_eq!(end.last.state, State::Verified, "{seen:?}");
    assert!(matches!(
        seen[0],
        (State::RefusedArtifact, Action::Commit { replace: true, .. })
    ));
    assert_eq!(rig.chain.submits.load(Ordering::SeqCst), 2);
    assert_eq!(rig.mock.session_state(&old).as_deref(), Some("refused"));
}

/// A commit step that finds a commitment of this state directory counting
/// which its diagnosis did not see (another run committed meanwhile) is not
/// progress: it neither commits again nor lets a watch loop read again at
/// once.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_commitment_that_already_counts_is_not_progress() {
    let rig = Rig::new(true).await;
    rig.feed("none");
    let (end, _) = run(&rig, &opts(true)).await;
    assert_eq!(end.last.state, State::Verified);
    let env = rig.env();
    let mut seen = resolve::observe(&env).await.unwrap();
    // The diagnosis read the chain before that commitment landed.
    seen.chain.as_mut().unwrap().commitment = None;
    let mut d = resolve::diagnose(&env, &seen, &opts(true).policy);
    d.action = Action::Commit {
        cohort: 7,
        replace: true,
    };
    let outcome = resolve::act(&env, &seen, &d, &opts(true).policy).await;
    assert!(
        matches!(&outcome, resolve::Outcome::Transient(t) if t.contains("already counts")),
        "{outcome:?}"
    );
    assert_eq!(rig.chain.submits.load(Ordering::SeqCst), 1);
    // Within the margin of the cutoff at the fresh head: refused just
    // before signing, whatever the diagnosis said.
    let mut d = resolve::diagnose(&env, &seen, &opts(true).policy);
    d.action = Action::Commit {
        cohort: 7,
        replace: true,
    };
    d.cutoff_block = Some(rig.mock.with(|s| s.finalized_block) + 50);
    let mut other = env;
    other.artifact = Some(packed(rig._dir.path(), 0x24, "{}"));
    let outcome = resolve::act(&other, &seen, &d, &opts(true).policy).await;
    assert!(
        matches!(&outcome, resolve::Outcome::Failed(t) if t.contains("never risks")),
        "{outcome:?}"
    );
    assert_eq!(rig.chain.submits.load(Ordering::SeqCst), 1);
}
