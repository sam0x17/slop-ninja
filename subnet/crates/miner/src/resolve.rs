//! `resolve`: reads every source of one hotkey's intake state, diagnoses it
//! ([`crate::intake`]) and takes the one safe action the diagnosis allows:
//! commit (only from the chain's facts, before the cutoff's landing bound),
//! open or resume the upload with fresh URLs and follow its verification,
//! or repair missing recorded objects. Transient trouble (status API or
//! coordinator outages, `paused`, RPC lag) is waited out with a doubling
//! backoff.
//!
//! Every step is idempotent: a commitment is made only when the chain holds
//! none of this state directory's openings for the cohort; the upload
//! session is the envelope's digest, so a rerun finds it; repairs mint URLs
//! for exactly the missing keys. Each decision and its outcome is appended
//! to `resolve-log.jsonl` in the state directory (mode 0600; no nonce, URL
//! or signature), and one lock file keeps two runs on one state directory
//! from acting at once.

use std::future::Future;
use std::io::Write as _;
use std::os::unix::fs::OpenOptionsExt as _;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use serde::Serialize;
use slop_ninja_admission_wire::ErrorCode;
use slop_ninja_admission_wire::json::{CohortInfo, HotkeyView};
use slop_ninja_protocol::submission::SUBMISSION_PAYLOAD_LEN;
use slop_ninja_protocol::{AccountId, ChainBinding};
use url::Url;

use crate::client::{AdmissionClient, ApiError, RequestSigner, Transfer, cohort_info, ss58};
use crate::direct::PutPolicy;
use crate::intake::{self, Action, ChainFacts, Diagnosis, Inputs, Policy, State};
use crate::object::ObjectHeader;
use crate::opening::{OpeningFile, OsNonce, Store, Submitted, load_opening};
use crate::status_api::{self, ApiStatus, ReadError, Which};
use crate::upload::{UploadOptions, UploadOutcome};

/// Chain access for one hotkey: its finalized facts and its commitment
/// submission. The CLI implements it with rtao; tests with the mock's log.
pub trait ChainAccess {
    /// Binds the access to the subnet the coordinator serves, before any
    /// read or submission (its cohort read names it).
    fn bind(&self, _netuid: u16) {}
    fn facts(&self, hotkey: &AccountId) -> impl Future<Output = Result<ChainFacts>>;
    /// Signs and submits the commitment with the hotkey and waits for its
    /// finalized inclusion.
    fn submit(
        &self,
        payload: &[u8; SUBMISSION_PAYLOAD_LEN],
    ) -> impl Future<Output = Result<Submitted>>;
}

/// A chain the run does not read (status only).
pub struct NoChain;

impl ChainAccess for NoChain {
    async fn facts(&self, _: &AccountId) -> Result<ChainFacts> {
        bail!("no chain access in this run")
    }
    async fn submit(&self, _: &[u8; SUBMISSION_PAYLOAD_LEN]) -> Result<Submitted> {
        bail!("no chain access in this run")
    }
}

/// Where the public status comes from.
#[derive(Clone, Debug)]
pub struct ApiSource {
    pub origin: Url,
    pub network: String,
    pub mechanism: String,
    pub which: Which,
}

/// Everything one run reads and acts with.
pub struct Env<'a, C: ChainAccess> {
    pub http: reqwest::Client,
    pub api: Option<ApiSource>,
    /// The coordinator's admission origin (already chosen against the trust file).
    pub coordinator: Option<Url>,
    /// The hotkey's signer, for the signed view, uploads and repairs.
    pub signer: Option<Arc<dyn RequestSigner>>,
    pub put_policy: PutPolicy,
    pub transfer: Transfer,
    pub chain: Option<&'a C>,
    pub store: &'a Store,
    pub hotkey: AccountId,
    /// The object named on the command line, and its header.
    pub artifact: Option<(PathBuf, ObjectHeader)>,
    pub upload: UploadOptions,
    /// Unix seconds now (a clock the tests can fix).
    pub now: fn() -> u64,
}

/// What one run read, with the reason each missing source is missing.
#[derive(Default)]
pub struct Observed {
    pub api: Option<ApiStatus>,
    pub api_error: Option<ReadError>,
    pub cohort_read: Option<CohortInfo>,
    pub cohort_error: Option<String>,
    pub view: Option<HotkeyView>,
    pub view_note: Option<String>,
    /// The signed routes answered `not_found` or `forbidden` for this hotkey.
    pub signed_closed: bool,
    pub chain: Option<ChainFacts>,
    pub chain_error: Option<String>,
    pub openings: Vec<(PathBuf, OpeningFile)>,
}

impl Observed {
    /// Whether a source that answered before is failing now in a way a
    /// later read can fix.
    pub fn transient(&self) -> bool {
        self.api_error.as_ref().is_some_and(ReadError::transient)
            || self.cohort_error.is_some()
            || self.chain_error.is_some()
    }

    pub fn notes(&self) -> Vec<String> {
        let mut out = Vec::new();
        if let Some(e) = &self.cohort_error {
            out.push(format!("the coordinator's cohort read failed: {e}"));
        }
        if let Some(e) = &self.view_note {
            out.push(e.clone());
        }
        if let Some(e) = &self.chain_error {
            out.push(format!("the chain was not read: {e}"));
        }
        out
    }
}

pub fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// Reads every source. A source that fails is recorded, never fatal: the
/// diagnosis works from what answered.
pub async fn observe<C: ChainAccess>(env: &Env<'_, C>) -> Result<Observed> {
    let mut seen = Observed::default();
    let hotkey = ss58(&env.hotkey);
    if let Some(api) = &env.api {
        match status_api::fetch(
            &env.http,
            &api.origin,
            &api.network,
            &api.mechanism,
            &api.which,
        )
        .await
        {
            Ok(status) => seen.api = Some(status),
            Err(e) => seen.api_error = Some(e),
        }
    }
    if let Some(base) = &env.coordinator {
        match cohort_info(&env.http, base).await {
            Ok(info) => seen.cohort_read = Some(info),
            Err(e) => seen.cohort_error = Some(e.to_string()),
        }
    }
    let cohort = seen
        .cohort_read
        .as_ref()
        .and_then(|r| r.admitting.as_ref().map(|a| a.cohort))
        .or_else(|| seen.api.as_ref().and_then(|a| a.cohort.as_ref()?.cohort));
    if let (Some(cohort), Some(mut client)) = (cohort, connect(env).await) {
        match client.hotkey_view(cohort).await {
            Ok(view) => seen.view = Some(view),
            Err(e) => {
                seen.signed_closed =
                    matches!(e.code(), Some(ErrorCode::NotFound | ErrorCode::Forbidden));
                seen.view_note = Some(match e.code() {
                    Some(ErrorCode::NotFound | ErrorCode::Forbidden) => {
                        "the coordinator's signed view of this hotkey is not open to it yet \
                         (allow-listed hotkeys only until admission opens to every miner)"
                            .to_owned()
                    }
                    Some(ErrorCode::NotRegistered) => {
                        "the upload Worker answered not_registered for this hotkey".to_owned()
                    }
                    _ => format!("the coordinator's signed view was not read: {e}"),
                });
            }
        }
    }
    if let Some(chain) = env.chain {
        if let Some(read) = &seen.cohort_read {
            chain.bind(read.chain.netuid);
        }
        match chain.facts(&env.hotkey).await {
            Ok(facts) => seen.chain = Some(facts),
            Err(e) => seen.chain_error = Some(format!("{e:#}")),
        }
    }
    let netuid = seen.cohort_read.as_ref().map(|r| r.chain.netuid);
    seen.openings = env
        .store
        .all_openings()?
        .into_iter()
        .filter(|(_, f)| f.hotkey == hotkey && netuid.is_none_or(|n| n == f.netuid))
        .collect();
    Ok(seen)
}

/// A signed admission client, when a signer and coordinator are given and
/// the coordinator answers.
async fn connect<C: ChainAccess>(env: &Env<'_, C>) -> Option<AdmissionClient> {
    let (base, signer) = (env.coordinator.clone()?, env.signer.clone()?);
    let (client, _) = AdmissionClient::connect(env.http.clone(), base, signer, None)
        .await
        .ok()?;
    Some(
        client
            .with_transfer(env.transfer)
            .with_put_policy(env.put_policy.clone()),
    )
}

/// Diagnoses what `observe` read.
pub fn diagnose<C: ChainAccess>(env: &Env<'_, C>, seen: &Observed, policy: &Policy) -> Diagnosis {
    let hotkey = ss58(&env.hotkey);
    let mut d = intake::diagnose(
        &Inputs {
            hotkey: &hotkey,
            api: seen.api.as_ref(),
            api_error: seen.api_error.as_ref(),
            cohort_read: seen.cohort_read.as_ref(),
            view: seen.view.as_ref(),
            chain: seen.chain.as_ref(),
            openings: &seen.openings,
            artifact: env.artifact.as_ref().map(|(_, h)| h),
            signed_closed: seen.signed_closed,
            now: (env.now)(),
        },
        policy,
    );
    d.warnings.extend(seen.notes());
    d
}

/// One line of `resolve-log.jsonl`.
#[derive(Clone, Debug, Serialize)]
pub struct LogEntry<'a> {
    pub schema: &'static str,
    pub at: u64,
    pub hotkey: &'a str,
    pub cohort: Option<u64>,
    pub state: &'static str,
    pub action: &'a Action,
    /// `taken`, `done`, `skipped`, `blocked`, `waiting`, `transient` or `failed`.
    pub outcome: &'a str,
    pub detail: &'a str,
}

pub const LOG_SCHEMA: &str = "slop_ninja.miner.resolve_log/v1";
pub const LOG_FILE: &str = "resolve-log.jsonl";
pub const LOCK_FILE: &str = "resolve.lock";

/// Appends one entry (mode 0600; a log group or others can read is refused).
pub fn log(store: &Store, entry: &LogEntry<'_>) -> Result<()> {
    let path = store.root().join(LOG_FILE);
    if path.exists() {
        crate::opening::check_private(&path)?;
    }
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .mode(0o600)
        .open(&path)
        .with_context(|| format!("open {}", path.display()))?;
    let mut line = serde_json::to_vec(entry)?;
    line.push(b'\n');
    file.write_all(&line)?;
    Ok(())
}

/// The state directory's lock, held while a run may act.
pub fn lock(store: &Store) -> Result<std::fs::File> {
    let path = store.root().join(LOCK_FILE);
    let file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .mode(0o600)
        .open(&path)
        .with_context(|| format!("open {}", path.display()))?;
    match file.try_lock() {
        Ok(()) => Ok(file),
        Err(std::fs::TryLockError::WouldBlock) => bail!(
            "another resolve or watch is acting on state directory {}; let it finish",
            store.root().display()
        ),
        Err(std::fs::TryLockError::Error(e)) => Err(e).context("lock the state directory"),
    }
}

/// How an action ended.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Outcome {
    /// Nothing to take.
    Idle,
    /// Taken; the state moved on (read again to see where).
    Done(String),
    /// Not taken now; a later run may (transient trouble, feed lag, a wait).
    Transient(String),
    /// Refused for good, or failed in a way a rerun does not fix.
    Failed(String),
}

/// Whether an admission error goes away by waiting.
fn api_transient(error: &anyhow::Error) -> bool {
    error.chain().any(|cause| {
        cause
            .downcast_ref::<ApiError>()
            .is_some_and(ApiError::retryable)
    })
}

/// Takes the diagnosis's action. Re-checks the facts it rests on first.
pub async fn act<C: ChainAccess>(
    env: &Env<'_, C>,
    seen: &Observed,
    d: &Diagnosis,
    policy: &Policy,
) -> Outcome {
    let result = match &d.action {
        Action::None | Action::Wait => return Outcome::Idle,
        Action::Commit { cohort, replace } => {
            commit(env, seen, *cohort, *replace, d.cutoff_block, policy).await
        }
        Action::Upload { opening } => upload(env, opening).await,
        Action::Repair { session } => repair(env, session).await,
    };
    match result {
        Ok(outcome) => outcome,
        Err(error) if api_transient(&error) => Outcome::Transient(format!("{error:#}")),
        Err(error) => Outcome::Failed(format!("{error:#}")),
    }
}

async fn commit<C: ChainAccess>(
    env: &Env<'_, C>,
    seen: &Observed,
    cohort: u64,
    replace: bool,
    cutoff_block: Option<u64>,
    policy: &Policy,
) -> Result<Outcome> {
    let chain = env.chain.context("no chain access")?;
    let (_, header) = env.artifact.as_ref().context("no --artifact")?;
    let read = seen.cohort_read.as_ref().context("no cohort read")?;
    let binding: ChainBinding = read
        .chain
        .binding()
        .context("the coordinator reported a malformed chain")?;
    // The commitment goes to the subnet the coordinator serves.
    chain.bind(binding.netuid);
    // The facts again, just before signing: a commitment this directory can
    // open for this artifact that counts now, and that the diagnosis did not
    // see, means another run committed meanwhile. The one the diagnosis saw
    // is the one it means to replace (a refused or stale commitment).
    let facts = chain.facts(&env.hotkey).await?;
    // The landing bound again, at the fresh finalized head: the
    // commitment's era ends within the margin, so it can land only before
    // the cutoff's earliest block.
    match cutoff_block {
        Some(cut)
            if facts
                .finalized_block
                .saturating_add(policy.commit_margin_blocks)
                < cut => {}
        Some(cut) => {
            return Ok(Outcome::Failed(format!(
                "the cutoff's earliest block {cut} is now within {} blocks of the finalized head {}: \
                 the client never risks a commitment after the cutoff",
                policy.commit_margin_blocks, facts.finalized_block
            )));
        }
        None => bail!("the cutoff block is not known: not committing"),
    }
    if let Some(genesis) = facts.genesis {
        anyhow::ensure!(
            genesis == binding.genesis,
            "the RPC endpoint serves another chain than the coordinator"
        );
    }
    let identity = facts
        .registration
        .context("the hotkey is not registered at the finalized head")?;
    let observed = seen.chain.as_ref().and_then(|c| c.commitment.as_ref());
    if let Some((block, payload)) = &facts.commitment
        && observed != facts.commitment.as_ref()
        && intake::payload_cohort(payload) == Some(cohort)
        && read
            .admitting
            .as_ref()
            .is_some_and(|a| *block >= a.open_block)
    {
        let hex = hex::encode(payload);
        let ours = env
            .store
            .all_openings()?
            .into_iter()
            .any(|(_, f)| f.chain_payload == hex && f.cohort == cohort && f.fits(header));
        if ours {
            // Not progress: the diagnosis that asked for a commitment was
            // stale (another run committed meanwhile, or a source lags), so
            // the next pass reads again after the usual wait, never at once.
            return Ok(Outcome::Transient(format!(
                "a commitment of this state directory for this artifact already counts for cohort \
                 {cohort} (block {block}); not committing again"
            )));
        }
    }
    let prepared = crate::commit::prepare(
        env.store,
        &binding,
        cohort,
        &identity,
        header,
        &mut OsNonce,
        replace,
    )?;
    let submitted = chain.submit(&prepared.payload).await;
    let submitted = match submitted {
        Ok(s) => s,
        Err(error) => {
            // The opening stays: if the call landed after all, the next
            // read finds its payload on chain and uploads it.
            return Ok(Outcome::Transient(format!(
                "the commitment was not confirmed ({error:#}); its opening {} is kept, and the next \
                 run reads the chain before committing again",
                prepared.path.display()
            )));
        }
    };
    let mut record = prepared.file.clone();
    record.submitted = Some(submitted.clone());
    env.store.update_opening(&prepared.path, &record)?;
    Ok(Outcome::Done(format!(
        "committed for cohort {cohort}: finalized commitment block {}; opening {}",
        submitted
            .finalized_commitment_block
            .map_or_else(|| "?".to_owned(), |b| b.to_string()),
        prepared.path.display()
    )))
}

async fn upload<C: ChainAccess>(env: &Env<'_, C>, opening_path: &Path) -> Result<Outcome> {
    let (path, header) = env.artifact.as_ref().context("no --artifact")?;
    let Some(mut client) = connect(env).await else {
        return Ok(Outcome::Transient(
            "the coordinator did not answer (or no hotkey signer): the upload waits".into(),
        ));
    };
    let mut opening = load_opening(opening_path)?;
    if let Some(migrated) = opening.migrated(header) {
        env.store.update_opening(opening_path, &migrated)?;
        opening = migrated;
    }
    match crate::upload::upload(&mut client, env.store, &opening, path, header, &env.upload).await?
    {
        UploadOutcome::Interrupted {
            uploaded,
            remaining,
        } => Ok(Outcome::Transient(format!(
            "stopped after {uploaded} objects; {remaining} remain"
        ))),
        UploadOutcome::Finished(session) => {
            let text = crate::upload::describe(&session);
            Ok(match session.state() {
                Some(slop_ninja_admission_wire::SessionState::Refused) => Outcome::Failed(text),
                Some(slop_ninja_admission_wire::SessionState::Verifying) => {
                    Outcome::Transient(text)
                }
                _ => Outcome::Done(text),
            })
        }
    }
}

async fn repair<C: ChainAccess>(env: &Env<'_, C>, session: &str) -> Result<Outcome> {
    let (path, header) = env.artifact.as_ref().context("no --artifact")?;
    let Some(mut client) = connect(env).await else {
        return Ok(Outcome::Transient(
            "the coordinator did not answer (or no hotkey signer): the repair waits".into(),
        ));
    };
    let after = crate::upload::repair(&mut client, session, path, header, &env.upload).await?;
    let text = crate::upload::describe(&after);
    Ok(if after.repair_needed.is_empty() {
        Outcome::Done(text)
    } else {
        Outcome::Transient(text)
    })
}

/// How a run proceeds.
#[derive(Clone, Copy, Debug)]
pub struct RunOptions {
    pub policy: Policy,
    /// Take actions (`resolve`); `false` only diagnoses (`status`).
    pub act: bool,
    /// Keep going until the state settles, a manual step is needed or
    /// `max_wait` passes; else one pass.
    pub watch: bool,
    /// Wait between reads when nothing went wrong: the status API's cache life.
    pub refresh: Duration,
    /// The longest backoff: one tempo at the nominal block time.
    pub max_backoff: Duration,
    pub max_wait: Option<Duration>,
}

/// How a run ended.
#[derive(Clone, Debug)]
pub struct RunEnd {
    pub last: Diagnosis,
    /// A manual step is needed (the diagnosis's `blocked`, or a refusal).
    pub manual: bool,
    pub failed: Option<String>,
}

/// Runs passes until the run ends. `show` gets each diagnosis and outcome.
pub async fn run<C: ChainAccess>(
    env: &Env<'_, C>,
    opts: &RunOptions,
    mut show: impl FnMut(&Diagnosis, Option<&Outcome>),
) -> Result<RunEnd> {
    let _lock = if opts.act {
        Some(lock(env.store)?)
    } else {
        None
    };
    let started = tokio::time::Instant::now();
    let mut backoff = opts.refresh;
    let mut last_progress: Option<(State, Action)> = None;
    loop {
        let seen = observe(env).await?;
        let d = diagnose(env, &seen, &opts.policy);
        let hotkey = ss58(&env.hotkey);
        let outcome = if opts.act {
            Some(act(env, &seen, &d, &opts.policy).await)
        } else {
            None
        };
        show(&d, outcome.as_ref());
        if opts.act {
            let (label, detail) = match (&outcome, &d.blocked) {
                (Some(Outcome::Idle), Some(why)) => ("blocked", why.clone()),
                (Some(Outcome::Idle), None) if d.action == Action::Wait => {
                    ("waiting", d.next.clone())
                }
                (Some(Outcome::Idle), None) => ("skipped", d.next.clone()),
                (Some(Outcome::Done(t)), _) => ("done", t.clone()),
                (Some(Outcome::Transient(t)), _) => ("transient", t.clone()),
                (Some(Outcome::Failed(t)), _) => ("failed", t.clone()),
                (None, _) => ("skipped", String::new()),
            };
            log(
                env.store,
                &LogEntry {
                    schema: LOG_SCHEMA,
                    at: (env.now)(),
                    hotkey: &hotkey,
                    cohort: d.cohort,
                    state: d.state.code(),
                    action: &d.action,
                    outcome: label,
                    detail: &detail,
                },
            )?;
        }
        let transient = seen.transient()
            || matches!(outcome, Some(Outcome::Transient(_)))
            || matches!(d.state, State::StatusUnavailable | State::Paused);
        // Progress moves the state: the same state and action after a step
        // that reported progress is not progress again (no zero-wait loop).
        let step = (d.state, d.action.clone());
        let progressed =
            matches!(outcome, Some(Outcome::Done(_))) && last_progress.as_ref() != Some(&step);
        last_progress = matches!(outcome, Some(Outcome::Done(_))).then_some(step);
        let failed = match &outcome {
            Some(Outcome::Failed(t)) => Some(t.clone()),
            _ => None,
        };
        // A step only the miner can take; one blocked by a source that is
        // down for now is a wait, not a manual step.
        let manual = d.blocked.is_some() && d.action == Action::None && !transient;
        let end = |last: Diagnosis| RunEnd {
            last,
            manual,
            failed: failed.clone(),
        };
        if !opts.watch {
            return Ok(end(d));
        }
        if failed.is_some() || d.state.settled() || manual {
            return Ok(end(d));
        }
        if d.action == Action::None && !transient && d.state != State::Unknown && !progressed {
            // Nothing the client can do and nothing pending: stop.
            return Ok(end(d));
        }
        let wait = if progressed {
            // The state moved: read again at once (a commit is followed by
            // its upload in the next pass).
            backoff = opts.refresh;
            Duration::ZERO
        } else if transient {
            let w = backoff;
            backoff = (backoff * 2).min(opts.max_backoff);
            w
        } else {
            backoff = opts.refresh;
            opts.refresh
        };
        if let Some(max) = opts.max_wait
            && started.elapsed() + wait > max
        {
            return Ok(end(d));
        }
        tokio::time::sleep(wait).await;
    }
}
