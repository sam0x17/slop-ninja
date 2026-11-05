//! The application stage after startup: verify the coordinator's signed
//! package, derive the native row with the protocol crate, choose its
//! owner-row policy and refuse (or defer) a row rtao would hold under it, then
//! either build an unsigned payload without touching the journal (the default
//! dry run) or enqueue it as `WeightWork` with that policy and dispatch
//! through rtao's journaled scheduler (`--dispatch`).
//!
//! The owner-row policy (rtao D-093) is `Submit` only for a reward-v1 owner
//! burn row: the launch burn row under the launch authorization, or a verified
//! cohort target whose row pays only the owner UID. Every other row, and every
//! `testnet-reward-v0` row, is queued under `Hold`. The hold decision is rtao's
//! own predicate, `OwnerRowPolicy::holds`, under the policy the work is queued
//! with, so self-only and self-plus-owner rows stay held.
//!
//! The dry run never enqueues: rtao's durable queue refuses any other
//! operation for a mechanism until the pending one is reconciled, and only
//! this application's own journaled retirement removes zero-attempt work, so
//! a mistaken package checked by a dry run must not occupy the journal.
//!
//! Every step is idempotent across restarts. The operation ID binds chain,
//! validator, manifest, cohort, revision and mechanism, so a restart at any
//! boundary re-derives the same row and joins the retained operation instead
//! of creating a second one. Signed attempts are rtao's: a crash or lost
//! response after send leaves an unresolved attempt that only finalized
//! recovery may classify, and resume is refused until it does.

use std::collections::BTreeMap;

use anyhow::{Context, Result};
use rtao_primitives::Uid;
use serde::Serialize;
use slop_ninja_protocol::{ChainBinding, RegistrationSnapshot, TrustedKeys};
use slop_ninja_subnet::{Enqueued, OwnerRowPolicy, WeightWork};

use crate::faults::{Faults, Phase};
use crate::package::{self, Bundle, PackageSummary, Refusal};
use crate::payload::DryRunReport;
use crate::startup::FinalizedHead;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Mode {
    /// Build the unsigned payload and print its hash. Never loads a key.
    DryRun,
    /// Load the configured hotkey and dispatch through the journal.
    Dispatch,
}

/// Entries a dispatch report keeps per list: as many as rtao's reveal
/// watcher retains outcomes. A continuous run logs every entry when it
/// happens; the report keeps the most recent ones.
pub const REPORT_RECENT: usize = 256;

/// Redacted dispatch result. The scalar commit fields describe the latest
/// commit; `commits` lists the most recent commits of the target this run
/// observed ([`REPORT_RECENT`]).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct DispatchReport {
    /// `once` or `per_tempo`.
    pub recommit: String,
    pub finalized_blocks: u32,
    /// Attempts and unresolved attempts over every retained operation of the target.
    pub attempts: usize,
    pub unresolved_attempts: usize,
    pub finalized_success_block: Option<u64>,
    pub outstanding_reveals: usize,
    /// Read back from `TimelockedWeightCommits` once the commit was finalized.
    pub commit_epoch: Option<u64>,
    pub reveal_epoch: Option<u64>,
    pub reveal_round: Option<u64>,
    /// `Weights(netuid, uid)` read at finality once the reveal was observed.
    pub chain_weights: Vec<(u16, u16)>,
    /// Whether those weights equal the derived row exactly.
    pub chain_weights_match: Option<bool>,
    /// Per-tempo re-commits this run enqueued.
    pub tempo_enqueued: Vec<(u64, String)>,
    /// Per-tempo re-commits deferred this run (label, code): a row that must
    /// burn while the owner hotkey holds no UID, or one rtao would hold.
    pub tempo_deferred: Vec<(u64, String)>,
    pub commits: Vec<CommitRecord>,
    /// Reveals compared with their own operation's committed row.
    pub reveals_compared: u64,
    /// Reveals whose epoch ended without a success event.
    pub reveals_missed: u64,
    /// Reveals no recorded row accounts for (observed only).
    pub reveals_unattributed: u64,
    /// Attempts on the tracked operation that rtao reported `Failed` with
    /// nothing awaiting classification (D-095: an unsigned failure adds no
    /// journal attempt and is retried on its own), such as an
    /// admission-window overrun; counted once per attempt.
    pub unsigned_failures: u64,
    /// Times the tracked operation stayed unsigned (no new journal attempt)
    /// for two epochs: at least one commit window passed (alerted).
    pub windows_without_attempt: u64,
    /// The last fetch session's outcome code, when `[fetch]` is configured.
    pub last_fetch: String,
    pub stop: String,
}

/// One finalized commit of the target, read back at finality. Redacted: no
/// row, ciphertext, signed bytes or addresses.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct CommitRecord {
    pub operation_id: String,
    /// `None` for the target's first commit.
    pub tempo: Option<u64>,
    pub attempts: usize,
    pub finalized_success_block: u64,
    pub commit_epoch: u64,
    pub reveal_epoch: u64,
    /// Block and drand round recorded in this hotkey's chain entry.
    pub commit_block: u64,
    pub reveal_round: u64,
    /// This hotkey's entries in `TimelockedWeightCommits(commit_epoch)` at
    /// finality. More than one is a duplicate commit and halts the run.
    pub hotkey_entries: usize,
    /// `Weights` equal the derived row after this commit's reveal.
    pub weights_match: Option<bool>,
}

/// Subnet epoch label of the next per-tempo re-commit: the epoch after the
/// last commit this hotkey made for the mechanism, past any label already
/// retained. rtao's cadence never signs twice in one epoch, so the commit
/// itself lands in that epoch or, after an outage, the first one rtao reaches.
pub fn next_tempo(last_commit_epoch: u64, retained: &[Option<u64>]) -> Result<u64> {
    let mut label = last_commit_epoch
        .checked_add(1)
        .context("epoch label overflows")?;
    for _ in 0..=retained.len() {
        if !retained.contains(&Some(label)) {
            return Ok(label);
        }
        label = label.checked_add(1).context("epoch label overflows")?;
    }
    anyhow::bail!("no free per-tempo label")
}

/// Effects of the application stage. The live implementation wraps the
/// adapter and client opened by startup; tests substitute fakes.
#[allow(async_fn_in_trait)]
pub trait AppPorts {
    fn read_bundle(&mut self) -> Result<Vec<u8>>;
    /// Whether a `slop_ninja.fixture_package/v1` bundle (the T12
    /// `testnet-reward-v0` package, which a validator verified receipt by
    /// receipt and recomputed) is still applied. Never by a release build:
    /// validators copy the coordinator's signed row and do nothing else, so
    /// such a bundle is refused like the retired reward-v1 schemas
    /// (`bundle_schema_retired`, a deferral, never a commit). Only the
    /// crate's own journal and dispatch tests, which use the fixture as a
    /// signed input, turn it on.
    fn fixture_packages(&self) -> bool {
        false
    }
    fn trusted_keys(&self) -> Result<TrustedKeys>;
    /// Genesis and netuid this validator serves.
    fn chain(&self) -> ([u8; 32], u16);
    /// `(uid, hotkey, registration_block)` for every registered UID at `head`.
    async fn registration_snapshot(
        &mut self,
        head: &FinalizedHead,
        chain: &ChainBinding,
    ) -> Result<RegistrationSnapshot>;
    /// This validator's own hotkey and `SubnetOwnerHotkey(netuid)` at `head`,
    /// read by name: the hotkeys rtao's owner-row guard decides from.
    async fn hold_hotkeys(&mut self, head: &FinalizedHead) -> Result<Vec<[u8; 32]>>;
    /// The subnet's weight limits as rtao's preparation applies them, read
    /// with rtao's own weight preflight at the best head (see
    /// [`WeightLimits`]). Read only for a row queued under
    /// [`OwnerRowPolicy::Submit`].
    async fn weight_limits(&mut self, mechanism: u8) -> Result<WeightLimits>;
    /// The chain's subnet epoch index in finalized state (reward-v1 tempos).
    async fn epoch_index(&mut self, head: &FinalizedHead) -> Result<u64>;
    /// The operation ID this work would have, without touching the journal.
    fn operation_id(&self, work: &WeightWork) -> Result<String>;
    /// Whether the journal already retains this operation.
    fn retained(&self, operation_id: &str) -> Result<bool>;
    /// Queue the work under `owner_rows`, journaled with it.
    fn enqueue(
        &mut self,
        work: &WeightWork,
        input: BTreeMap<Uid, f64>,
        owner_rows: OwnerRowPolicy,
    ) -> Result<Enqueued>;
    /// Allow the signing guard to sign this operation with this row, under
    /// the owner-row policy this process chose for it.
    fn authorize(&mut self, operation_id: &str, input: &BTreeMap<Uid, f64>, owner_rows: OwnerRowChoice) -> Result<()>;
    /// reward-v1: this validator's durable record of the target it holds
    /// active; `None` before it accepted any bundle (the launch burn row).
    fn held_target(&self) -> Result<Option<crate::package_v2::HeldTargetFile>>;
    /// Every signed revocation this validator journaled ([`crate::revocations`]),
    /// as checked when journaled. None by default.
    fn revocations(&self) -> Result<crate::revocations::Loaded> {
        Ok(crate::revocations::Loaded::default())
    }
    /// Durably records the target held active after a verified bundle.
    fn record_target(&mut self, held: &crate::package_v2::HeldTargetFile) -> Result<()>;
    /// Operations of `mechanism` that rtao has not handed back, any target.
    fn pending_operations(&self, mechanism: u8) -> Result<Vec<slop_ninja_subnet::PendingOperation>>;
    /// Journaled retirement of a pending operation that never landed and
    /// never can; false when it is not retirable.
    fn retire(&mut self, operation_id: &str, reason: &str) -> Result<bool>;
    /// Row hash and hand-back state of a retained operation.
    fn retained_row(&self, operation_id: &str) -> Result<Option<(String, bool)>>;
    /// The owner-row policy a retained operation was queued under.
    fn retained_policy(&self, operation_id: &str) -> Result<Option<OwnerRowPolicy>>;
    /// Why this application retired a retained operation, if it did.
    fn retired(&self, operation_id: &str) -> Result<Option<String>>;
    async fn dry_run(
        &mut self,
        operation_id: &str,
        derived: &package::Derived,
    ) -> Result<DryRunReport>;
    async fn dispatch(
        &mut self,
        operation_id: &str,
        derived: &package::Derived,
        faults: &Faults,
    ) -> Result<DispatchReport>;
}

// One value per process run; boxing would only complicate matching.
#[allow(clippy::large_enum_variant)]
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum AppOutcome {
    /// Dry run: the package verified and the commit was built, unsigned. The
    /// journal was not written; `retained` says whether dispatch would join
    /// an operation already in it.
    DryRun {
        package: PackageSummary,
        operation_id: String,
        retained: bool,
        row_digest: String,
        entries: Vec<(u16, u16)>,
        /// The owner-row policy dispatch would queue the row under.
        owner_rows: OwnerRowPolicy,
        dry_run: DryRunReport,
    },
    /// Dispatch: the package verified and its row is retained in the journal.
    Enqueued {
        package: PackageSummary,
        operation_id: String,
        created: bool,
        row_digest: String,
        entries: Vec<(u16, u16)>,
        dispatch: DispatchReport,
    },
    /// The package or its row was refused; nothing was enqueued.
    Refused { code: &'static str, detail: String },
    /// reward-v1: nothing to commit at this tempo, by rule rather than by a
    /// failed check: the row must burn while the owner hotkey holds no UID
    /// (`row_deferred`), rtao would hold the row under its owner-row policy
    /// (`row_would_be_held`: a self-only or self-plus-owner row, or an
    /// owner-only row that is not a burn row), the subnet's
    /// `MinAllowedWeights` refuses a one-entry burn row from this non-owner
    /// validator (`burn_row_min_allowed_weights`), a burn row would list
    /// another UID under a `MaxWeightsLimit` below 65535
    /// (`burn_row_weights_limit`), this non-owner validator holds no
    /// validator permit the chain would require of its burn row at reveal
    /// (`burn_row_no_permit`), the authorization does not cover this
    /// epoch (`authorization_refused`: the target is not the held one or the
    /// epoch precedes the authorization; never an expiry), a journaled
    /// signed revocation withdraws it (`authorization_revoked`, until a newer
    /// authorization), a
    /// pending operation still awaits recovery (`pending_operation_unsettled`),
    /// the installed bundle is of a retired schema (`bundle_schema_retired`:
    /// an upgrade over a v1 or v3 bundle; nothing commits until a
    /// coordinator-row bundle is installed),
    /// or, with `[fetch]`, a fetched v3 bundle's referenced cache files are
    /// missing or changed (`referenced_object_missing`,
    /// `referenced_object_mismatch`: logged `bundle_cache_unreadable` with
    /// `alert = true`; the next fetch session rebuilds the bundle in full).
    /// The process exits for a later retry (75); the journal is unchanged
    /// apart from retiring superseded work that never landed.
    Deferred { code: &'static str, detail: String },
}

/// Refusal codes that defer a tempo rather than halt the validator.
pub fn is_deferral(code: &str) -> bool {
    is_row_deferral(code)
        || matches!(
            code,
            "authorization_refused" | "authorization_revoked" | "pending_operation_unsettled" | "bundle_schema_retired"
        )
}

/// Whether a dispatch run that ended with `stop` exits for a later retry
/// (75): every tempo deferral (`tempo_deferred:<code>`), among them held work
/// retired in the loop ([`crate::live::OWNER_ROW_HELD_STOP`]).
pub fn stop_retries(stop: &str) -> bool {
    stop.starts_with("tempo_deferred:")
}

/// Deferrals about the row itself: a later snapshot or chain state is tried
/// afresh at the next tempo.
pub fn is_row_deferral(code: &str) -> bool {
    matches!(
        code,
        "row_deferred"
            | "row_would_be_held"
            | "burn_row_min_allowed_weights"
            | "burn_row_weights_limit"
            | "burn_row_no_permit"
    )
}

/// Row deferrals that leave this validator committing nothing at a tempo
/// while the chain and the package would let a row be derived: a row rtao
/// would hold, or a burn row the chain's limits or permit refuse. Each is
/// logged with `alert = true`, since a validator that defers every tempo
/// otherwise looks healthy.
pub fn alerts_on_deferral(code: &str) -> bool {
    code == "row_would_be_held" || code.starts_with("burn_row_")
}

/// Log a tempo deferral: a warning with `alert = true` for the codes
/// [`alerts_on_deferral`] names, else information.
pub fn log_tempo_deferred(tempo: Option<u64>, code: &str) {
    if alerts_on_deferral(code) {
        tracing::warn!(event = "tempo_deferred", tempo, code, alert = true);
    } else {
        tracing::info!(event = "tempo_deferred", tempo, code);
    }
}

fn deferred(refusal: Refusal) -> AppOutcome {
    log_tempo_deferred(None, refusal.code);
    AppOutcome::Deferred {
        code: refusal.code,
        detail: refusal.detail,
    }
}

fn refused_or_deferred(refusal: Refusal) -> AppOutcome {
    if is_deferral(refusal.code) { deferred(refusal) } else { refused(refusal) }
}

fn refused(refusal: Refusal) -> AppOutcome {
    tracing::warn!(event = "package_refused", code = refusal.code);
    AppOutcome::Refused {
        code: refusal.code,
        detail: refusal.detail,
    }
}

/// Verify, derive, enqueue, then dry-run or dispatch, at one finalized head.
pub async fn apply<P: AppPorts>(
    ports: &mut P,
    head: &FinalizedHead,
    mode: Mode,
    faults: &Faults,
) -> Result<AppOutcome> {
    let bytes = ports.read_bundle().context("package bundle unreadable")?;
    if crate::package_v2::is_v2(&bytes) {
        return apply_v2(ports, head, mode, faults, &bytes).await;
    }
    if !ports.fixture_packages() {
        return Ok(deferred(Refusal {
            code: "bundle_schema_retired",
            detail: "a fixture package bundle (receipts and a score package to recompute) is retired: validators \
                     commit the coordinator's signed row; install a coordinator-row bundle (the fetcher does)"
                .into(),
        }));
    }
    let bundle = match Bundle::parse(&bytes) {
        Ok(bundle) => bundle,
        Err(refusal) => return Ok(refused(refusal)),
    };
    let keys = ports.trusted_keys()?;
    let (genesis, netuid) = ports.chain();
    let verified = match package::verify(&bundle, &keys, &genesis, netuid) {
        Ok(verified) => verified,
        Err(refusal) => return Ok(refused(refusal)),
    };
    let summary = verified.summary();
    tracing::info!(
        event = "package_verified",
        cohort = summary.cohort,
        revision = summary.revision,
        receipts = summary.receipt_count,
        miners = summary.scored_miners
    );
    faults.hit(Phase::PackageVerified)?;

    let snapshot = ports
        .registration_snapshot(head, verified.chain())
        .await
        .context("registration snapshot read failed")?;
    let derived = match package::derive(&verified, &snapshot) {
        Ok(derived) => derived,
        Err(refusal) => return Ok(refused(refusal)),
    };
    tracing::info!(
        event = "row_derived",
        entries = derived.row_entries.len(),
        registration_block = snapshot.block
    );
    faults.hit(Phase::RowDerived)?;

    // A `testnet-reward-v0` row is queued under `Hold`: rtao never commits
    // one whose positive entries are only this validator's UID or the owner
    // hotkey's UID, so its operation would stay pending forever and block
    // every later package.
    let hold = ports
        .hold_hotkeys(head)
        .await
        .context("owner hotkey read failed")?;
    let (own, owner) = own_and_owner(&hold)?;
    if row_held(OwnerRowPolicy::Hold, &derived.entries(), &snapshot, own, owner) {
        return Ok(refused(Refusal {
            code: "row_would_be_held",
            detail: "every positive entry is this validator's UID or the owner hotkey's UID; rtao holds such a row and never commits it".into(),
        }));
    }

    match mode {
        Mode::DryRun => {
            let operation_id = ports.operation_id(&derived.work)?;
            let retained = ports.retained(&operation_id)?;
            let report = ports.dry_run(&operation_id, &derived).await?;
            tracing::info!(
                event = "dry_run_payload",
                signing_payload_sha256 = %report.signing_payload_sha256,
                reveal_round = report.reveal_round,
                retained,
                signed = false
            );
            Ok(AppOutcome::DryRun {
                package: summary,
                operation_id,
                retained,
                row_digest: derived.row_digest(),
                entries: derived.entries(),
                owner_rows: OwnerRowPolicy::Hold,
                dry_run: report,
            })
        }
        Mode::Dispatch => {
            let enqueued = ports
                .enqueue(&derived.work, derived.input.clone(), OwnerRowPolicy::Hold)
                .context("enqueue refused")?;
            let (operation_id, created) = match enqueued {
                Enqueued::Created(id) => (id, true),
                Enqueued::Existing(id) => (id, false),
            };
            tracing::info!(event = "row_enqueued", created);
            faults.hit(Phase::Enqueued)?;
            ports.authorize(&operation_id, &derived.input, OwnerRowChoice::Hold)?;
            let dispatch = ports.dispatch(&operation_id, &derived, faults).await?;
            Ok(AppOutcome::Enqueued {
                package: summary,
                operation_id,
                created,
                row_digest: derived.row_digest(),
                entries: derived.entries(),
                dispatch,
            })
        }
    }
}

/// The reward-v1 path. The bundle is verified against the target this
/// validator holds active in its own durable record (the launch burn row
/// before any), and the target active after it is recorded before anything
/// else, so a superseded bundle or the launch authorization after the first
/// activation is refused from then on. The row is the coordinator's signed
/// row (the copy model): its entries exactly, under the authorization
/// checked for the finalized epoch. This validator's own finalized snapshot
/// and owner hotkey decide only the owner-row policy and the burn-row
/// limits, never a weight; a row that must wait is deferred, never a halt.
///
/// Dispatch first settles retained work ([`derive_queueable`]): an operation
/// of this target and label with the same row and owner-row policy is
/// joined; one with another row or policy that never landed is retired and
/// the next label is used; then every other pending operation of the
/// mechanism (a superseded target, a missed tempo) that never landed is
/// retired. Only then is the current operation enqueued, so a pending
/// re-commit never wedges a target switch.
async fn apply_v2<P: AppPorts>(
    ports: &mut P,
    head: &FinalizedHead,
    mode: Mode,
    faults: &Faults,
    bytes: &[u8],
) -> Result<AppOutcome> {
    use crate::package_v2::{HeldTargetFile, advances};
    let keys = ports.trusted_keys()?;
    let (genesis, netuid) = ports.chain();
    let chain = slop_ninja_protocol::ChainBinding {
        genesis: slop_ninja_protocol::Digest(genesis),
        netuid,
        mechanism: 0,
    };
    let held = match ports.held_target()? {
        Some(file) => match file.restore(&chain) {
            Ok(held) => held,
            Err(refusal) => return Ok(refused(refusal)),
        },
        None => slop_ninja_protocol::ActiveTarget::launch(&chain).map_err(|e| anyhow::anyhow!("{e}"))?,
    };
    let authorized = match crate::package_v2::verify(bytes, &keys, &genesis, netuid, &held) {
        Ok(a) => std::sync::Arc::new(a),
        // An installed bundle of a retired schema (an upgrade over a v1 or
        // v3 bundle): nothing new is committed until a row bundle is
        // installed, but the process, its recovery and its fetcher keep
        // running (a continuous run waits for the fetcher; a one-shot run
        // exits 75 for a later retry), never a halt.
        Err(refusal) if refusal.code == "bundle_schema_retired" => {
            tracing::warn!(
                event = "bundle_schema_retired",
                hint = "install a coordinator-row bundle (the fetcher, or the operator's row mirror); nothing new commits until then",
                alert = true
            );
            return Ok(deferred(refusal));
        }
        Err(refusal) => return Ok(refused(refusal)),
    };
    let loaded = match ports.revocations() {
        Ok(loaded) => loaded,
        Err(error) => {
            // No revocations loaded yet (startup, or a re-apply before any
            // load): refuse rather than commit on a check that did not run.
            // The process exits and its supervisor restarts it, alerting
            // each time until the store is fixed.
            tracing::warn!(event = "revocation_store_unreadable", error = %format!("{error:#}"), kept = 0, alert = true);
            return Err(error.context("revocation store read failed (fix or restore the store; nothing commits until it reads)"));
        }
    };
    if let Some(error) = &loaded.store_error {
        tracing::warn!(event = "revocation_store_unreadable", error = %error, kept = loaded.records.len(), alert = true);
    }
    for problem in &loaded.problems {
        tracing::warn!(event = "revocation_entry_unreadable", problem = %problem, alert = true);
    }
    authorized.set_revocations(loaded.records);
    let active = authorized.verified.active.clone();
    if active != held {
        if !advances(&held, &active) {
            return Ok(refused(Refusal {
                code: "held_target_regressed",
                detail: "the bundle would move the held target backwards".into(),
            }));
        }
        ports.record_target(&HeldTargetFile::of(&active)).context("held target write failed")?;
        tracing::info!(event = "held_target_recorded", target = %active.digest(), activated_epoch = ?active.activated_epoch());
    }
    let summary = authorized.summary();
    tracing::info!(
        event = "package_verified",
        rule = "reward-v2",
        cohort = summary.cohort,
        target = %summary.target,
        row_entries = summary.scored_miners
    );
    faults.hit(Phase::PackageVerified)?;
    if let Some(revocation) = authorized.revoked_by() {
        // A signed revocation withdrew this authorization: no commitment of
        // its target is queued, and queued work of it that never signed is
        // retired. Work with a signed attempt is left to recovery. A newer
        // authorization resumes commitments.
        let retired = if mode == Mode::Dispatch {
            retire_revoked(ports, authorized.verified.chain.mechanism, authorized.verified.target_digest().0)?
        } else {
            0
        };
        tracing::warn!(
            event = "authorization_revoked",
            target = %summary.target,
            revocation = %revocation,
            retired,
            alert = true
        );
        return Ok(deferred(Refusal {
            code: "authorization_revoked",
            detail: format!("revocation {revocation} withdraws the installed authorization"),
        }));
    }
    let chain = authorized.verified.chain.clone();
    let v1 = ports
        .registration_snapshot(head, &chain)
        .await
        .context("registration snapshot read failed")?;
    let hold = ports.hold_hotkeys(head).await.context("owner hotkey read failed")?;
    let (own, owner) = own_and_owner(&hold)?;
    let snapshot = match crate::package_v2::snapshot(&v1, owner) {
        Ok(s) => s,
        Err(refusal) => return Ok(refused(refusal)),
    };
    let epoch = ports.epoch_index(head).await.context("epoch index read failed")?;
    let mechanism = authorized.verified.chain.mechanism;
    // The label starts at the finalized epoch; a retained operation of this
    // target and label that cannot be joined moves it on.
    let (derived, policy, label) =
        match derive_queueable(ports, authorized.clone(), &snapshot, &v1, own, owner, epoch, mode).await? {
            Ok(found) => found,
            Err(refusal) => return Ok(refused_or_deferred(refusal)),
        };
    tracing::info!(
        event = "row_derived",
        rule = "reward-v2",
        entries = derived.row_entries.len(),
        registration_block = snapshot.block,
        epoch,
        label,
        row_digest = %derived.row_digest,
        owner_rows = ?policy
    );
    faults.hit(Phase::RowDerived)?;
    match mode {
        Mode::DryRun => {
            let operation_id = ports.operation_id(&derived.work)?;
            let retained = ports.retained(&operation_id)?;
            let report = ports.dry_run(&operation_id, &derived).await?;
            Ok(AppOutcome::DryRun {
                package: summary,
                operation_id,
                retained,
                row_digest: derived.row_digest(),
                entries: derived.entries(),
                owner_rows: policy,
                dry_run: report,
            })
        }
        Mode::Dispatch => {
            let operation_id = ports.operation_id(&derived.work)?;
            for pending in ports.pending_operations(mechanism)? {
                if pending.id == operation_id {
                    continue;
                }
                if !pending.retirable() || !ports.retire(&pending.id, "superseded")? {
                    return Ok(deferred(Refusal {
                        code: "pending_operation_unsettled",
                        detail: "a pending operation of another target or tempo awaits recovery".into(),
                    }));
                }
                tracing::info!(event = "operation_retired", reason = "superseded", tempo = ?pending.work.and_then(|w| w.tempo));
            }
            let enqueued =
                ports.enqueue(&derived.work, derived.input.clone(), policy).context("enqueue refused")?;
            let (operation_id, created) = match enqueued {
                Enqueued::Created(id) => (id, true),
                Enqueued::Existing(id) => (id, false),
            };
            faults.hit(Phase::Enqueued)?;
            ports.authorize(&operation_id, &derived.input, OwnerRowChoice::of(policy, &v1, own, owner))?;
            let dispatch = ports.dispatch(&operation_id, &derived, faults).await?;
            Ok(AppOutcome::Enqueued {
                package: summary,
                operation_id,
                created,
                row_digest: derived.row_digest(),
                entries: derived.entries(),
                dispatch,
            })
        }
    }
}

/// `(own hotkey, owner hotkey)` from [`AppPorts::hold_hotkeys`].
pub fn own_and_owner(hold: &[[u8; 32]]) -> Result<([u8; 32], [u8; 32])> {
    let own = *hold.first().context("own hotkey missing")?;
    let owner = *hold.get(1).context("owner hotkey missing")?;
    Ok((own, owner))
}

/// The subnet's weight limits as rtao's preparation applies them (its
/// `conform`), and the validator permit the chain checks, read with rtao's
/// weight preflight at the best head (compiled reads, by name when the
/// compiled shapes no longer match).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct WeightLimits {
    /// `MinAllowedWeights(netuid)`.
    pub min_allowed_weights: u16,
    /// `MaxWeightsLimit(netuid)`, a share over 65535.
    pub max_weights_limit: u16,
    /// `SubnetworkN(netuid)`.
    pub population: u16,
    /// `ValidatorPermit(netuid)` at this validator's UID; false when it
    /// holds no UID or no permit.
    pub own_permit: bool,
    /// `SubnetEpochConsensus(netuid)` is `Null` rather than Yuma.
    pub null_consensus: bool,
}

/// The owner-row policy this process chose for an operation, recorded with
/// the signing guard's authorization: the guard refuses an intent journaled
/// under another policy, and a `Submit` intent whose row pays any UID but
/// `owner_uid`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case", tag = "policy")]
pub enum OwnerRowChoice {
    Hold,
    /// A burn row: every positive entry is the UID the finalized owner
    /// hotkey holds in the snapshot the row was derived from;
    /// `owner_validator` when this validator's hotkey is the owner hotkey.
    Submit { owner_uid: u16, owner_validator: bool },
}

impl OwnerRowChoice {
    /// The choice for a row queued under `policy`, derived from `snapshot`
    /// with the owner hotkey `owner`. `Submit` without an owner UID cannot
    /// be a burn row ([`burn_row`]); it is recorded as `Hold`, which the
    /// guard then refuses to sign under `Submit`.
    pub fn of(policy: OwnerRowPolicy, snapshot: &RegistrationSnapshot, own: [u8; 32], owner: [u8; 32]) -> Self {
        match (policy, uid_of(snapshot, &owner)) {
            (OwnerRowPolicy::Submit, Some(owner_uid)) => Self::Submit { owner_uid, owner_validator: own == owner },
            _ => Self::Hold,
        }
    }

    pub fn policy(&self) -> OwnerRowPolicy {
        match self {
            Self::Hold => OwnerRowPolicy::Hold,
            Self::Submit { .. } => OwnerRowPolicy::Submit,
        }
    }
}

impl WeightLimits {
    /// The count of nonzero entries rtao's `conform` requires of any row but
    /// a lone self-weight: `MinAllowedWeights` capped at the population.
    pub fn effective_min_allowed_weights(&self) -> u16 {
        self.min_allowed_weights.min(self.population)
    }
}

/// Which owner burn row a derived row is, when it is one this validator
/// queues under [`OwnerRowPolicy::Submit`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum BurnRow {
    /// The launch burn row, verified under the launch authorization.
    LaunchBurn,
    /// A verified cohort target whose row pays only the owner UID: its units
    /// burn, or an entrant registered under the owner hotkey is paid there
    /// (the chain receives the same row either way).
    BurnTarget,
}

/// The UID `hotkey` holds in `snapshot`.
fn uid_of(snapshot: &RegistrationSnapshot, hotkey: &[u8; 32]) -> Option<u16> {
    snapshot
        .neurons
        .iter()
        .find(|neuron| &neuron.hotkey.0 == hotkey)
        .map(|neuron| neuron.uid)
}

/// The burn row kind of `derived`, or `None`. Only a reward-v1 row, which
/// [`crate::package_v2::derive`] produced under an authorization it checked
/// for this epoch, qualifies, and only when it has a positive entry and every
/// positive entry goes to the UID the finalized owner hotkey holds. Every
/// `testnet-reward-v0` row is `None`.
pub fn burn_row(derived: &crate::package::Derived, snapshot: &RegistrationSnapshot, owner: [u8; 32]) -> Option<BurnRow> {
    let authorized = derived.reward_v1.as_ref()?;
    let owner_uid = uid_of(snapshot, &owner)?;
    let entries = derived.chain_entries();
    let mut positive = entries.iter().filter(|(_, weight)| *weight > 0).peekable();
    positive.peek()?;
    if !positive.all(|(uid, _)| *uid == owner_uid) {
        return None;
    }
    Some(if authorized.verified.row.activated_epoch.is_none() { BurnRow::LaunchBurn } else { BurnRow::BurnTarget })
}

/// Whether rtao's scheduler would hold `entries` under `policy`: its own
/// predicate, [`OwnerRowPolicy::holds`], decided the way its scheduler
/// decides. This validator's UID is the owner's exactly when its own hotkey is
/// the owner hotkey (`SubnetOwnerHotkey`, read by name), never from a UID map;
/// any other UID is the owner's when the finalized snapshot maps it to the
/// owner hotkey. A hotkey with no UID in the snapshot has no own entry.
pub fn row_held(
    policy: OwnerRowPolicy,
    entries: &[(u16, u16)],
    snapshot: &RegistrationSnapshot,
    own: [u8; 32],
    owner: [u8; 32],
) -> bool {
    // Rows only list UIDs below the population (at most 4096), so the
    // sentinel never equals a destination.
    let own_uid = Uid(uid_of(snapshot, &own).unwrap_or(u16::MAX));
    policy.holds(
        entries.iter().map(|&(uid, weight)| (Uid(uid), weight)),
        own_uid,
        |dest| {
            if dest == own_uid {
                own == owner
            } else {
                snapshot
                    .neurons
                    .iter()
                    .find(|neuron| neuron.uid == dest.0)
                    .is_some_and(|neuron| neuron.hotkey.0 == owner)
            }
        },
    )
}

/// The chain limits a burn row must meet before it is queued under
/// [`OwnerRowPolicy::Submit`], since rtao journals work before it prepares
/// it and a row its `conform` refuses would stay pending:
///
/// - this non-owner validator's one-entry burn row needs an effective
///   `MinAllowedWeights` of 1 (`burn_row_min_allowed_weights`); an owner
///   validator's row to its own UID is a lone self-weight, which `conform`
///   exempts;
/// - under a `MaxWeightsLimit` below 65535, the row lists only the owner UID
///   (`burn_row_weights_limit`), since `conform` would spread a listed zero's
///   share. Reward-v1 rows list positive entries only, so a burn row always
///   does;
/// - this validator holds a validator permit wherever the chain's
///   `check_validator_permit` requires one (`burn_row_no_permit`): always
///   under `Null` consensus, and under Yuma unless it is the owner validator
///   or the row is its lone self-weight. rtao only warns about a missing
///   permit, and for a timelocked commit the chain applies the check again
///   at reveal, where a failure drops the row without any error; the reveal
///   ledger would see that one reveal period later as `weights_mismatch`.
///
/// The limits and the permit must stay compatible from commit through
/// reveal: the chain checks the row's length and the permit again when it
/// reveals the commit, one or more epochs later. Neither is re-read between
/// commit and reveal here.
pub fn check_burn_row(
    entries: &[(u16, u16)],
    snapshot: &RegistrationSnapshot,
    own: [u8; 32],
    owner: [u8; 32],
    limits: &WeightLimits,
) -> std::result::Result<(), Refusal> {
    check_burn_limits(entries, uid_of(snapshot, &owner), own == owner, limits)
}

/// [`check_burn_row`] given the owner UID and whether this validator is the
/// owner validator.
pub fn check_burn_limits(
    entries: &[(u16, u16)],
    owner_uid: Option<u16>,
    owner_validator: bool,
    limits: &WeightLimits,
) -> std::result::Result<(), Refusal> {
    let self_weight = owner_validator && entries.len() == 1 && Some(entries[0].0) == owner_uid;
    let minimum = limits.effective_min_allowed_weights();
    if !self_weight && minimum > 1 {
        return Err(Refusal::new(
            "burn_row_min_allowed_weights",
            format!(
                "the subnet requires {minimum} nonzero weights from a validator that is not the owner; a burn row has one, so this tempo is deferred rather than committed"
            ),
        ));
    }
    if limits.max_weights_limit < u16::MAX && entries.iter().any(|(uid, _)| Some(*uid) != owner_uid) {
        return Err(Refusal::new(
            "burn_row_weights_limit",
            "under a MaxWeightsLimit below 65535 a burn row must list only the owner UID; the chain limits would spread the other entries' share",
        ));
    }
    // The chain's `check_validator_permit`: under Yuma a lone self-weight and
    // the owner validator are exempt; under Null consensus nobody is.
    let permit_exempt = !limits.null_consensus && (owner_validator || self_weight);
    if !permit_exempt && !limits.own_permit {
        return Err(Refusal::new(
            "burn_row_no_permit",
            "this validator holds no validator permit, which the chain requires of its burn row when it reveals the commit; this tempo is deferred rather than committed",
        ));
    }
    Ok(())
}

/// The owner-row policy `derived` is queued under, with its burn row kind,
/// or `row_would_be_held`: `Submit` for a [`burn_row`], `Hold` for every
/// other row; either way a row rtao would hold under that policy is
/// deferred. Pure: the chain limits a burn row must meet are checked
/// separately ([`burn_row_limits`]), once it is known whether the journal
/// already retains the operation.
pub fn owner_row_choice(
    derived: &crate::package::Derived,
    snapshot: &RegistrationSnapshot,
    own: [u8; 32],
    owner: [u8; 32],
) -> std::result::Result<(OwnerRowPolicy, Option<BurnRow>), Refusal> {
    let burn = burn_row(derived, snapshot, owner);
    let policy = if burn.is_some() { OwnerRowPolicy::Submit } else { OwnerRowPolicy::Hold };
    if row_held(policy, &derived.chain_entries(), snapshot, own, owner) {
        return Err(Refusal::new(
            "row_would_be_held",
            "every positive entry is this validator's UID or the owner hotkey's UID and the row is not an owner burn row; rtao holds such a row, so this tempo is deferred",
        ));
    }
    Ok((policy, burn))
}

/// [`check_burn_row`] against the limits read now ([`AppPorts::weight_limits`]).
pub async fn burn_row_limits<P: AppPorts>(
    ports: &mut P,
    derived: &crate::package::Derived,
    snapshot: &RegistrationSnapshot,
    own: [u8; 32],
    owner: [u8; 32],
    kind: BurnRow,
) -> Result<std::result::Result<(), Refusal>> {
    let limits = ports
        .weight_limits(derived.work.mechanism.0)
        .await
        .context("weight limits read failed")?;
    if let Err(refusal) = check_burn_row(&derived.chain_entries(), snapshot, own, owner, &limits) {
        return Ok(Err(refusal));
    }
    tracing::info!(event = "burn_row_submit", kind = ?kind, min_allowed_weights = limits.min_allowed_weights, max_weights_limit = limits.max_weights_limit, population = limits.population, own_permit = limits.own_permit, null_consensus = limits.null_consensus);
    Ok(Ok(()))
}

/// Retires every pending operation of `mechanism` for the revoked `target`
/// that never landed (`authorization_revoked`). One with a signed attempt
/// awaiting recovery, or a landed one, is left for recovery. Returns how many
/// were retired.
pub fn retire_revoked<P: AppPorts>(ports: &mut P, mechanism: u8, target: [u8; 32]) -> Result<usize> {
    let mut retired = 0;
    for pending in ports.pending_operations(mechanism)? {
        if pending.work.as_ref().and_then(|w| w.v2_target) != Some(target) || !pending.retirable() {
            continue;
        }
        if ports.retire(&pending.id, "authorization_revoked")? {
            tracing::info!(event = "operation_retired", reason = "authorization_revoked", tempo = ?pending.work.and_then(|w| w.tempo));
            retired += 1;
        }
    }
    Ok(retired)
}

/// Retires every pending `Submit` operation of `mechanism` with the row
/// `row_hash` that never landed, after the chain limits refused that burn
/// row (`burn_row_limits_changed`): rtao's preparation would refuse it
/// before signing on every attempt and keep the mechanism's queue occupied.
/// One with a signed attempt awaiting recovery, or a landed one, is left
/// for recovery. Returns how many were retired.
fn retire_refused_burn_rows<P: AppPorts>(ports: &mut P, mechanism: u8, row_hash: &str) -> Result<usize> {
    let mut retired = 0;
    for pending in ports.pending_operations(mechanism)? {
        if pending.row_hash.as_deref() != Some(row_hash)
            || !pending.retirable()
            || ports.retained_policy(&pending.id)? != Some(OwnerRowPolicy::Submit)
        {
            continue;
        }
        if ports.retire(&pending.id, "burn_row_limits_changed")? {
            tracing::info!(event = "operation_retired", reason = "burn_row_limits_changed", tempo = ?pending.work.and_then(|w| w.tempo));
            retired += 1;
        }
    }
    Ok(retired)
}

/// reward-v1: the row this validator queues at the first label from
/// `label` on that the journal admits, its owner-row policy and that label,
/// or the refusal that defers (or refuses) the tempo. Shared by the first
/// commitment ([`apply_v2`]) and the per-tempo re-commit, so both decide the
/// same way.
///
/// The hold decision ([`owner_row_choice`]) comes first. A dry run retains
/// nothing, so a burn row then meets the chain limits read now. In dispatch
/// the label's retained operation is looked up before any limits read:
///
/// - none: a burn row must meet the limits read now; if they refuse it, the
///   tempo is deferred (`burn_row_*`) and pending `Submit` work of the same
///   row that never landed is retired (`burn_row_limits_changed`);
/// - retired by this application: never joined; the next label is tried;
/// - the same row under the same policy: it is joined whatever the limits
///   read now, so a restart adopts its unresolved attempt or reads its landed
///   commit back. Only a pending burn row that never landed and that the
///   limits now refuse is retired (`burn_row_limits_changed`) and the tempo
///   deferred;
/// - the same row under another policy (a burn row an earlier build queued
///   under `Hold`; rtao refuses the same ID under another policy), or another
///   row (a re-derived snapshot): a pending one that never landed is retired
///   (`owner_row_policy_changed`, `row_rederived`) and the next label is
///   tried; one awaiting recovery defers (`pending_operation_unsettled`); a
///   handed-back one only moves the label on.
#[allow(clippy::too_many_arguments)]
pub async fn derive_queueable<P: AppPorts>(
    ports: &mut P,
    authorized: std::sync::Arc<crate::package_v2::Authorized>,
    snapshot: &slop_ninja_protocol::RegistrationSnapshotV2,
    v1: &RegistrationSnapshot,
    own: [u8; 32],
    owner: [u8; 32],
    mut label: u64,
    mode: Mode,
) -> Result<std::result::Result<(crate::package::Derived, OwnerRowPolicy, u64), Refusal>> {
    loop {
        let derived = match crate::package_v2::derive(authorized.clone(), snapshot, own, label) {
            Ok(d) => d,
            Err(refusal) => return Ok(Err(refusal)),
        };
        let (policy, burn) = match owner_row_choice(&derived, v1, own, owner) {
            Ok(choice) => choice,
            Err(refusal) => return Ok(Err(refusal)),
        };
        if mode == Mode::DryRun {
            if let Some(kind) = burn
                && let Err(refusal) = burn_row_limits(ports, &derived, v1, own, owner, kind).await?
            {
                return Ok(Err(refusal));
            }
            return Ok(Ok((derived, policy, label)));
        }
        let mechanism = derived.work.mechanism.0;
        let id = ports.operation_id(&derived.work)?;
        let hash = slop_ninja_subnet::weight_row_hash(derived.input.clone())?;
        match ports.retained_row(&id)? {
            None => {
                if let Some(kind) = burn
                    && let Err(refusal) = burn_row_limits(ports, &derived, v1, own, owner, kind).await?
                {
                    retire_refused_burn_rows(ports, mechanism, &hash)?;
                    return Ok(Err(refusal));
                }
                return Ok(Ok((derived, policy, label)));
            }
            // Retired work never landed and is never joined: the next label.
            Some((_, true)) if ports.retired(&id)?.is_some() => {}
            Some((retained, handed_off)) if retained == hash => {
                if ports.retained_policy(&id)? == Some(policy) {
                    if let Some(kind) = burn
                        && !handed_off
                        && let Err(refusal) = burn_row_limits(ports, &derived, v1, own, owner, kind).await?
                    {
                        if ports.retire(&id, "burn_row_limits_changed")? {
                            tracing::info!(event = "operation_retired", reason = "burn_row_limits_changed", tempo = label);
                            retire_refused_burn_rows(ports, mechanism, &hash)?;
                            return Ok(Err(refusal));
                        }
                        // A signed attempt awaits recovery, or the commit
                        // landed: join it so recovery and read-back settle it.
                        tracing::info!(event = "burn_row_limits_changed_after_signing", code = refusal.code, tempo = label);
                    }
                    return Ok(Ok((derived, policy, label)));
                }
                if !handed_off {
                    if !ports.retire(&id, "owner_row_policy_changed")? {
                        return Ok(Err(Refusal::new(
                            "pending_operation_unsettled",
                            "a pending operation of this tempo under another owner-row policy awaits recovery",
                        )));
                    }
                    tracing::info!(event = "operation_retired", reason = "owner_row_policy_changed", tempo = label);
                }
            }
            Some((_, true)) => {}
            Some((_, false)) => {
                if !ports.retire(&id, "row_rederived")? {
                    return Ok(Err(Refusal::new(
                        "pending_operation_unsettled",
                        "a pending operation of this tempo with another row awaits recovery",
                    )));
                }
                tracing::info!(event = "operation_retired", reason = "row_rederived", tempo = label);
            }
        }
        label = label.checked_add(1).context("epoch label overflows")?;
    }
}
