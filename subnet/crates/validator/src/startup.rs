//! Startup ordering: open the journal (persisting pause), evaluate signed
//! operations against live metadata and record readiness, reconcile finalized
//! history without a signer, check the subnet, hotkey registration and
//! commit/reveal against finalized state, and only then resume explicitly.
//! Readiness is recorded before recovery because the adapter's resume gate
//! refuses any journal write after the recovery that established it, and a
//! first evaluation or a readiness change is such a write. Any refusal leaves
//! the journal paused. A runtime upgrade is never a refusal: the live-surface
//! stage records per-operation deferrals and continues. rtao reads its storage
//! through compiled descriptors while their shape matches and by name against
//! live metadata otherwise; the one runtime change that stops recovery is an
//! item it can read neither way. Startup has then already recorded the
//! `upstream_storage_refused` deferral and retries (exit 75).

use anyhow::{Context, Result};
use rtao_primitives::H256;
use serde::Serialize;

use crate::faults::{Faults, Phase};

/// Readiness code for an rtao storage read that neither its compiled
/// descriptor nor its by-name read accepts under live metadata.
pub const UPSTREAM_STORAGE_REFUSED: &str = "upstream_storage_refused";

/// One finalized block chosen for the whole startup sequence.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FinalizedHead {
    pub number: u64,
    pub hash: H256,
}

/// Redacted recovery counts. No addresses, rows, ciphertext or signed bytes:
/// finalized failures are named by operation ID, attempt number, inclusion
/// height and decoded error only.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct RecoveryCounts {
    pub ready_to_resume: bool,
    pub reveal_history_caught_up: bool,
    pub unresolved_attempts: usize,
    /// Decoded finalized failures awaiting review: recovery halts on any.
    pub finalized_failures: usize,
    /// Finalized failures whose receipt is not read yet: recovery stays
    /// incomplete and reads them again, without halting.
    pub failures_awaiting_decode: usize,
    /// Finalized failures reviewed or settled (evidence only).
    pub reviewed_failures: usize,
    pub semantic_contract_mismatches: usize,
    pub outstanding_reveals: usize,
    pub paused_during_recovery: bool,
    /// The failures counted in `finalized_failures`.
    pub review_required: Vec<slop_ninja_subnet::FailedAttempt>,
    /// Weight calls the chain refused that settled this round.
    pub refusals_settled: Vec<slop_ninja_subnet::SettledRefusal>,
    /// Failure receipts that could not be read this round.
    pub receipts_unreadable: Vec<slop_ninja_subnet::UnreadableReceipt>,
}

impl RecoveryCounts {
    /// The validator's view of one finalized recovery round.
    pub fn of(progress: &slop_ninja_subnet::RecoveryProgress) -> Self {
        Self {
            ready_to_resume: progress.ready_to_resume,
            reveal_history_caught_up: progress.reveal_history_caught_up,
            unresolved_attempts: progress.unresolved_attempts,
            finalized_failures: progress.finalized_failures,
            failures_awaiting_decode: progress.failures_awaiting_decode,
            reviewed_failures: progress.reviewed_failures,
            semantic_contract_mismatches: progress.semantic_contract_mismatches,
            outstanding_reveals: progress.outstanding_reveals.len(),
            paused_during_recovery: progress.paused_during_recovery,
            review_required: progress.review_required.clone(),
            refusals_settled: progress.failures_settled.refusals.clone(),
            receipts_unreadable: progress.failures_settled.unreadable.clone(),
        }
    }
}

/// Logs what recovery rounds found about finalized failed weight calls,
/// each as a fixed-name alert event. One per validator run (startup) or
/// dispatch loop, so a receipt read that keeps failing alerts once, not at
/// every head:
///
/// - `weight_commit_refused`: the chain refused a signed weight call with a
///   refusal that settles by itself (rate limit, too soon, stake below the
///   minimum). The validator commits again at a later tempo; a refusal that
///   recurs every tempo means it never commits, so it is an alert.
/// - `failure_receipt_unreadable`: a failure's receipt could not be read.
///   Signing stays paused until it is read. A passing class (`timeout`,
///   `transport`, ...) is read again at the next round; a lasting one
///   (`state_discarded`, `block_not_found`, `decode`, `metadata`) halts for
///   review. Alerted when first seen and again when its class changes; each
///   repeat is a debug-level `failure_receipt_unreadable_again`. Only the
///   fixed class is logged at alert level; the error text, which can carry
///   endpoint detail, goes to the debug-level `failure_receipt_read_error`.
/// - `finalized_failure_requires_review`: a decoded failure that does not
///   settle, whose receipt contradicts the journal, or whose receipt read
///   failed lastingly. Recovery halts with `recovery_requires_review` until
///   `review-failure` records a review.
#[derive(Debug, Default)]
pub struct FailureReadLog {
    /// Unreadable receipts alerted, by operation and attempt, with the class
    /// last alerted.
    unreadable: std::collections::BTreeMap<(String, u64), String>,
}

/// The remedy named in `failure_receipt_unreadable`.
const UNREADABLE_NEXT: &str = "signing is paused until this receipt is read; a passing failure is read again at the next head, a lasting one halts for review: point the validator at an endpoint holding the block's state and start it again, or stop the unit and run slop-ninja-journal review-failure on the attempt listed under review_required or awaiting_decode";

impl FailureReadLog {
    pub fn log(&mut self, counts: &RecoveryCounts, height: u64) {
        for refusal in &counts.refusals_settled {
            tracing::warn!(
                event = "weight_commit_refused",
                operation_id = %refusal.operation,
                attempt = refusal.attempt,
                tempo = ?refusal.tempo,
                block = refusal.block_number,
                error = %refusal.error,
                height,
                alert = true
            );
        }
        self.unreadable.retain(|(operation, attempt), _| {
            counts
                .receipts_unreadable
                .iter()
                .any(|read| &read.operation == operation && read.attempt == *attempt)
        });
        for unreadable in &counts.receipts_unreadable {
            tracing::debug!(
                event = "failure_receipt_read_error",
                operation_id = %unreadable.operation,
                attempt = unreadable.attempt,
                class = %unreadable.class,
                error = %unreadable.detail
            );
            let key = (unreadable.operation.clone(), unreadable.attempt);
            if self.unreadable.get(&key) == Some(&unreadable.class) {
                tracing::debug!(
                    event = "failure_receipt_unreadable_again",
                    operation_id = %unreadable.operation,
                    attempt = unreadable.attempt,
                    class = %unreadable.class,
                    height
                );
                continue;
            }
            self.unreadable.insert(key, unreadable.class.clone());
            tracing::warn!(
                event = "failure_receipt_unreadable",
                operation_id = %unreadable.operation,
                attempt = unreadable.attempt,
                block = unreadable.block_number,
                class = %unreadable.class,
                lasting = unreadable.lasting,
                signing = "paused",
                next = UNREADABLE_NEXT,
                height,
                alert = true
            );
        }
        for failure in &counts.review_required {
            tracing::warn!(
                event = "finalized_failure_requires_review",
                operation_id = %failure.operation,
                attempt = failure.attempt,
                block = failure.block_number,
                error = ?failure.error,
                receipt_contradiction = ?failure.receipt_contradiction,
                receipt_unavailable = ?failure.receipt_unavailable,
                height,
                alert = true
            );
        }
    }
}

/// Finalized subnet facts read together at one block.
///
/// `CommitRevealWeightsEnabled` defaults to `true` for a netuid that was never
/// written, so it is meaningful only once `subnet_exists` holds.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct SubnetState {
    /// `SubtensorModule::NetworksAdded(netuid)`.
    pub subnet_exists: bool,
    /// `SubtensorModule::Uids(netuid, hotkey)` is present.
    pub hotkey_registered: bool,
    /// `SubtensorModule::CommitRevealWeightsEnabled(netuid)`.
    pub commit_reveal_enabled: bool,
}

/// The effects startup needs. The live implementation wraps the native adapter
/// and an rtao client; tests substitute an offline fake.
#[allow(async_fn_in_trait)]
pub trait StartupPorts {
    /// Open the existing journal, persisting pause. Returns its chain genesis.
    fn open_journal(&mut self) -> Result<H256>;
    /// Connect read-only transport. Returns the connected chain's genesis.
    async fn connect(&mut self) -> Result<H256>;
    async fn finalized_head(&mut self) -> Result<FinalizedHead>;
    /// One bounded, signer-free `recover_finalized` call at `head`.
    async fn recover_finalized(&mut self, head: &FinalizedHead) -> Result<RecoveryCounts>;
    /// Subnet existence, hotkey registration and commit/reveal at `head`.
    async fn subnet_state(&mut self, head: &FinalizedHead) -> Result<SubnetState>;
    /// Evaluate signed operations against the live metadata and persist any
    /// readiness change. Failure here is reported, never a reason to halt.
    async fn runtime_surface(&mut self, head: &FinalizedHead) -> Result<SurfaceStatus>;
    /// Explicit resume; the adapter refuses it without in-process recovery.
    fn resume(&mut self) -> Result<()>;
}

/// Redacted live-surface readiness: operation names and reason codes only.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct SurfaceStatus {
    /// `Pallet.call` and reason code for each deferred operation.
    pub deferred: Vec<(String, String)>,
    /// Operations encoded through recorded safe adaptations.
    pub adapted: Vec<String>,
    /// Informational only; never compared for acceptance.
    pub observed_spec_version: Option<u32>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Stage {
    Connect,
    Recovery,
    CommitReveal,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum StartupOutcome {
    /// Every check passed and the journal accepted the explicit resume.
    Resumed {
        finalized_height: u64,
        /// Deferred operations (`Pallet.call`, code); also an alert in status.
        deferred_operations: Vec<(String, String)>,
    },
    /// Operator action is required; the journal stays paused.
    Halted { stage: Stage, code: &'static str },
    /// Retryable later from the same journal; the journal stays paused.
    Retry { stage: Stage, code: &'static str },
}

/// Run the startup sequence once. Errors are retryable; the journal remains paused.
pub async fn start<P: StartupPorts>(
    ports: &mut P,
    max_recovery_rounds: u32,
    faults: &Faults,
) -> Result<StartupOutcome> {
    let journal_genesis = ports.open_journal().context("journal open refused")?;
    tracing::info!(event = "journal_opened", paused = true);
    faults.hit(Phase::JournalOpened)?;

    let chain_genesis = ports.connect().await.context("RPC connection failed")?;
    if chain_genesis != journal_genesis {
        return Ok(halted(Stage::Connect, "chain_identity_mismatch"));
    }
    let head = ports
        .finalized_head()
        .await
        .context("finalized head read failed")?;
    tracing::info!(event = "finalized_head", height = head.number);

    // Record live-metadata readiness before recovery, as the adapter does
    // before each dispatch. A first evaluation or a readiness change writes
    // the journal, and the adapter's resume gate refuses any journal write
    // after the recovery that established it, so evaluating here keeps a
    // readiness change from refusing resume.
    let surface = match ports.runtime_surface(&head).await {
        Ok(surface) => {
            tracing::info!(
                event = "runtime_surface",
                observed_spec_version = surface.observed_spec_version,
                deferred = surface.deferred.len(),
                adapted = surface.adapted.len()
            );
            for (operation, code) in &surface.deferred {
                tracing::warn!(event = "operation_deferred", operation, code, alert = true);
            }
            Some(surface)
        }
        Err(error) => {
            // Signing re-resolves against live metadata at every attempt, so an
            // unreadable surface here defers nothing and stops nothing.
            tracing::warn!(event = "runtime_surface_unavailable", error = %error);
            None
        }
    };
    faults.hit(Phase::RuntimeSurfaceEvaluated)?;

    let mut ready = false;
    let mut failure_log = FailureReadLog::default();
    for round in 1..=max_recovery_rounds {
        let counts = match ports.recover_finalized(&head).await {
            Ok(counts) => counts,
            Err(error) => {
                // When rtao can read a storage item neither through its
                // compiled descriptor nor by name, the readiness just recorded
                // says so, and recovery cannot succeed until upstream supports
                // the live item: a classified retry with a durable alert, not
                // a crash.
                if surface.as_ref().is_some_and(|surface| {
                    surface
                        .deferred
                        .iter()
                        .any(|(_, code)| code == UPSTREAM_STORAGE_REFUSED)
                }) {
                    tracing::warn!(event = "recovery_refused_upstream", error = %error);
                    return Ok(retry(Stage::Recovery, UPSTREAM_STORAGE_REFUSED));
                }
                return Err(error.context("finalized recovery failed"));
            }
        };
        tracing::info!(event = "recovery_round", round, counts = ?counts);
        failure_log.log(&counts, head.number);
        faults.hit(Phase::RecoveryRound)?;
        if counts.finalized_failures != 0 || counts.semantic_contract_mismatches != 0 {
            return Ok(halted(Stage::Recovery, "recovery_requires_review"));
        }
        if counts.paused_during_recovery {
            return Ok(halted(Stage::Recovery, "paused_during_recovery"));
        }
        if counts.ready_to_resume {
            ready = true;
            break;
        }
    }
    if !ready {
        return Ok(retry(Stage::Recovery, "recovery_incomplete"));
    }

    let subnet = ports
        .subnet_state(&head)
        .await
        .context("subnet state read failed")?;
    tracing::info!(
        event = "commit_reveal_checked",
        subnet_exists = subnet.subnet_exists,
        hotkey_registered = subnet.hotkey_registered,
        enabled = subnet.commit_reveal_enabled
    );
    if !subnet.subnet_exists {
        return Ok(halted(Stage::CommitReveal, "subnet_not_registered"));
    }
    if !subnet.hotkey_registered {
        return Ok(halted(Stage::CommitReveal, "hotkey_not_registered"));
    }
    if !subnet.commit_reveal_enabled {
        return Ok(halted(Stage::CommitReveal, "commit_reveal_disabled"));
    }
    faults.hit(Phase::CommitRevealChecked)?;

    ports.resume().context("explicit resume refused")?;
    tracing::info!(event = "resumed");
    faults.hit(Phase::Resumed)?;
    Ok(StartupOutcome::Resumed {
        finalized_height: head.number,
        deferred_operations: surface.map(|surface| surface.deferred).unwrap_or_default(),
    })
}

fn halted(stage: Stage, code: &'static str) -> StartupOutcome {
    tracing::warn!(event = "startup_halted", stage = ?stage, code);
    StartupOutcome::Halted { stage, code }
}

fn retry(stage: Stage, code: &'static str) -> StartupOutcome {
    tracing::warn!(event = "startup_retry", stage = ?stage, code);
    StartupOutcome::Retry { stage, code }
}
