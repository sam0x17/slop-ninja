//! The application stage after startup: verify the coordinator's signed
//! package, derive the native row with the protocol crate, refuse a row rtao
//! would hold, then either build an unsigned payload without touching the
//! journal (the default dry run) or enqueue it as `WeightWork` and dispatch
//! through rtao's journaled scheduler (`--dispatch`).
//!
//! The dry run never enqueues: rtao's durable queue refuses any other
//! operation for a mechanism until the pending one is reconciled, and there is
//! no tool that removes zero-attempt work, so a mistaken package checked by a
//! dry run must not occupy the journal.
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
use slop_ninja_subnet::{Enqueued, WeightWork};

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
    fn trusted_keys(&self) -> Result<TrustedKeys>;
    /// Genesis and netuid this validator serves.
    fn chain(&self) -> ([u8; 32], u16);
    /// `(uid, hotkey, registration_block)` for every registered UID at `head`.
    async fn registration_snapshot(
        &mut self,
        head: &FinalizedHead,
        chain: &ChainBinding,
    ) -> Result<RegistrationSnapshot>;
    /// Hotkeys whose UIDs rtao treats as hold-only destinations: this
    /// validator's own hotkey and `SubnetOwnerHotkey(netuid)` at `head`.
    async fn hold_hotkeys(&mut self, head: &FinalizedHead) -> Result<Vec<[u8; 32]>>;
    /// The chain's subnet epoch index in finalized state (reward-v1 tempos).
    async fn epoch_index(&mut self, head: &FinalizedHead) -> Result<u64>;
    /// The operation ID this work would have, without touching the journal.
    fn operation_id(&self, work: &WeightWork) -> Result<String>;
    /// Whether the journal already retains this operation.
    fn retained(&self, operation_id: &str) -> Result<bool>;
    fn enqueue(&mut self, work: &WeightWork, input: BTreeMap<Uid, f64>) -> Result<Enqueued>;
    /// Allow the signing guard to sign this operation with this row.
    fn authorize(&mut self, operation_id: &str, input: &BTreeMap<Uid, f64>) -> Result<()>;
    /// reward-v1: this validator's durable record of the target it holds
    /// active; `None` before it accepted any bundle (the launch burn row).
    fn held_target(&self) -> Result<Option<crate::package_v2::HeldTargetFile>>;
    /// Durably records the target held active after a verified bundle.
    fn record_target(&mut self, held: &crate::package_v2::HeldTargetFile) -> Result<()>;
    /// Operations of `mechanism` that rtao has not handed back, any target.
    fn pending_operations(&self, mechanism: u8) -> Result<Vec<slop_ninja_subnet::PendingOperation>>;
    /// Journaled retirement of a pending operation that never landed and
    /// never can; false when it is not retirable.
    fn retire(&mut self, operation_id: &str, reason: &str) -> Result<bool>;
    /// Row hash and hand-back state of a retained operation.
    fn retained_row(&self, operation_id: &str) -> Result<Option<(String, bool)>>;
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
    /// (`row_deferred`), rtao would hold the row (`row_would_be_held`: its
    /// positive entries are only this validator's or the owner hotkey's UID,
    /// such as the launch burn row), the authorization does not cover this
    /// epoch (`authorization_refused`, until a renewal or a new bundle), or a
    /// pending operation still awaits recovery (`pending_operation_unsettled`).
    /// The process exits for a later retry (75); the journal is unchanged
    /// apart from retiring superseded work that never landed.
    Deferred { code: &'static str, detail: String },
}

/// Refusal codes that defer a tempo rather than halt the validator.
pub fn is_deferral(code: &str) -> bool {
    matches!(code, "row_deferred" | "row_would_be_held" | "authorization_refused" | "pending_operation_unsettled")
}

fn deferred(refusal: Refusal) -> AppOutcome {
    tracing::info!(event = "tempo_deferred", code = refusal.code);
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

    // rtao never commits a row whose positive entries are only this
    // validator's UID or the owner hotkey's UID (`HoldPrevious`), so its
    // operation would stay pending forever and block every later package.
    let hold = ports
        .hold_hotkeys(head)
        .await
        .context("owner hotkey read failed")?;
    if row_would_be_held(&derived.entries(), &snapshot, &hold) {
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
                dry_run: report,
            })
        }
        Mode::Dispatch => {
            let enqueued = ports
                .enqueue(&derived.work, derived.input.clone())
                .context("enqueue refused")?;
            let (operation_id, created) = match enqueued {
                Enqueued::Created(id) => (id, true),
                Enqueued::Existing(id) => (id, false),
            };
            tracing::info!(event = "row_enqueued", created);
            faults.hit(Phase::Enqueued)?;
            ports.authorize(&operation_id, &derived.input)?;
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
/// activation is refused from then on. The row is derived against this
/// validator's own finalized snapshot and owner hotkey for the finalized
/// epoch; a row that must wait is deferred, never a halt.
///
/// Dispatch first settles retained work: an operation of this target and
/// label with the same row is joined; one with another row (a re-derived
/// snapshot) that never landed is retired and the next label is used; every
/// other pending operation of the mechanism (a superseded target, a missed
/// tempo) that never landed is retired. Only then is the current operation
/// enqueued, so a pending re-commit never wedges a target switch.
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
        Err(refusal) => return Ok(refused(refusal)),
    };
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
        rule = "reward-v1",
        cohort = summary.cohort,
        target = %summary.target,
        receipts = authorized.receipts_verified,
        miners = summary.scored_miners
    );
    faults.hit(Phase::PackageVerified)?;
    let chain = authorized.verified.chain.clone();
    let v1 = ports
        .registration_snapshot(head, &chain)
        .await
        .context("registration snapshot read failed")?;
    let hold = ports.hold_hotkeys(head).await.context("owner hotkey read failed")?;
    let owner = *hold.get(1).context("owner hotkey missing")?;
    let own = *hold.first().context("own hotkey missing")?;
    let snapshot = match crate::package_v2::snapshot(&v1, owner) {
        Ok(s) => s,
        Err(refusal) => return Ok(refused(refusal)),
    };
    let epoch = ports.epoch_index(head).await.context("epoch index read failed")?;
    let mechanism = authorized.verified.chain.mechanism;
    // The label starts at the finalized epoch; a retained operation of this
    // target and label with another row moves it on (its row is immutable).
    let mut label = epoch;
    let derived = loop {
        let derived = match crate::package_v2::derive(authorized.clone(), &snapshot, own, label) {
            Ok(d) => d,
            Err(refusal) => return Ok(refused_or_deferred(refusal)),
        };
        if row_would_be_held(&derived.chain_entries(), &v1, &hold) {
            return Ok(deferred(Refusal {
                code: "row_would_be_held",
                detail: "every positive entry is this validator's UID or the owner hotkey's UID; rtao holds such a row, so this tempo is deferred".into(),
            }));
        }
        if mode == Mode::DryRun {
            break derived;
        }
        let id = ports.operation_id(&derived.work)?;
        let hash = slop_ninja_subnet::weight_row_hash(derived.input.clone())?;
        match ports.retained_row(&id)? {
            None => break derived,
            Some((retained, _)) if retained == hash => break derived,
            Some((_, true)) => {}
            Some((_, false)) => {
                if !ports.retire(&id, "row_rederived")? {
                    return Ok(deferred(Refusal {
                        code: "pending_operation_unsettled",
                        detail: "a pending operation of this tempo with another row awaits recovery".into(),
                    }));
                }
                tracing::info!(event = "operation_retired", reason = "row_rederived", tempo = label);
            }
        }
        label = label.checked_add(1).context("epoch label overflows")?;
    };
    tracing::info!(
        event = "row_derived",
        rule = "reward-v1",
        entries = derived.row_entries.len(),
        registration_block = snapshot.block,
        epoch,
        label,
        row_digest = %derived.row_digest
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
            let enqueued = ports.enqueue(&derived.work, derived.input.clone()).context("enqueue refused")?;
            let (operation_id, created) = match enqueued {
                Enqueued::Created(id) => (id, true),
                Enqueued::Existing(id) => (id, false),
            };
            faults.hit(Phase::Enqueued)?;
            ports.authorize(&operation_id, &derived.input)?;
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

/// Whether rtao's `hold_previous` would hold this row: every positive entry's
/// UID belongs to one of `hold` (own hotkey, owner hotkey) in `snapshot`.
pub fn row_would_be_held(
    entries: &[(u16, u16)],
    snapshot: &RegistrationSnapshot,
    hold: &[[u8; 32]],
) -> bool {
    entries
        .iter()
        .filter(|(_, weight)| *weight > 0)
        .all(|(uid, _)| {
            snapshot
                .neurons
                .iter()
                .find(|neuron| neuron.uid == *uid)
                .is_some_and(|neuron| hold.contains(&neuron.hotkey.0))
        })
}
