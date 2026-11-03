//! Startup ordering: open the journal (persisting pause), evaluate signed
//! operations against live metadata and record readiness, reconcile finalized
//! history without a signer, check the subnet, hotkey registration and
//! commit/reveal against finalized state, and only then resume explicitly.
//! Readiness is recorded before recovery because the adapter's resume gate
//! refuses any journal write after the recovery that established it, and a
//! first evaluation or a readiness change is such a write. Any refusal leaves
//! the journal paused. A runtime upgrade is never a refusal: the live-surface
//! stage records per-operation deferrals and continues. The one runtime change
//! that does stop recovery is upstream: rtao's scheduler reads its storage
//! through compiled descriptors, so a changed item makes recovery fail; startup
//! has already recorded the `upstream_storage_refused` deferral and retries
//! (exit 75).

use anyhow::{Context, Result};
use rtao_primitives::H256;
use serde::Serialize;

use crate::faults::{Faults, Phase};

/// Readiness code for an rtao storage read that no longer matches live metadata.
pub const UPSTREAM_STORAGE_REFUSED: &str = "upstream_storage_refused";

/// One finalized block chosen for the whole startup sequence.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FinalizedHead {
    pub number: u64,
    pub hash: H256,
}

/// Redacted recovery counts. No addresses, rows, ciphertext or signed bytes.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct RecoveryCounts {
    pub ready_to_resume: bool,
    pub reveal_history_caught_up: bool,
    pub unresolved_attempts: usize,
    pub finalized_failures: usize,
    pub semantic_contract_mismatches: usize,
    pub outstanding_reveals: usize,
    pub paused_during_recovery: bool,
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
    for round in 1..=max_recovery_rounds {
        let counts = match ports.recover_finalized(&head).await {
            Ok(counts) => counts,
            Err(error) => {
                // When rtao's typed storage reads no longer match live
                // metadata, the readiness just recorded says so, and recovery
                // cannot succeed until upstream reads them by name: that is a
                // classified retry with a durable alert, not a crash.
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
