//! Native durable weight delivery, separate from cohort scoring and evidence storage.
//!
//! The embedding application supplies authenticated, frozen rows and a semantic
//! guard. This crate does not calculate scores or implement a validator daemon.
//! Opening a scheduler pauses submissions. Recovery requires only a public client;
//! explicit resume and a caller-owned signing context are separate steps.
//!
//! No runtime spec or transaction version gates anything here. Each signing
//! attempt resolves its call by name in the live metadata (see [`surface`]); an
//! operation with no type-correct encoding is deferred alone, durably, and
//! re-evaluated on every later block. Cohorts, rows, operation IDs and the
//! journal identity are independent of the runtime and survive any number of
//! upgrades. Transaction signing itself uses the live spec and transaction
//! versions that rtao reads at signing time, as the chain requires.

use std::{collections::BTreeMap, path::Path, sync::Arc};

use anyhow::{Context, Result, bail, ensure};
use rtao_client::journal::{
    AttemptOutcome, Incompatibility, JournalControl, JournalError, OperationIntent,
    OperationJournal, SemanticGuard,
};
use rtao_client::{BlockHeader, BlockRef, Client, UnsignedExtrinsic, ViewExt};
use rtao_neuron::{
    Clock, ClockGuard, Metrics, NeuronCx, VersionKey, WeightCadence, WeightPolicy, WeightScheduler,
    WeightsInput, WeightsProposal,
};
/// rtao's owner-row policy (D-093), re-exported so the application decides
/// with the same predicate the scheduler applies.
pub use rtao_neuron::weights::OwnerRowPolicy;
use rtao_primitives::{AccountId, H256, MechId, NetUid, Uid};
use rtao_runtime::RuntimeHandle;
use rtao_signer::KeyView;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

pub mod surface;

const APPLICATION_KEY: &str = "slopninja.native-weights.v1";
const ROW_DOMAIN: &[u8] = b"slopninja.weight-input.v1\0";

/// Journal extension holding per-operation live-surface readiness.
pub const READINESS_KEY: &str = "slopninja.operation-readiness.v1";
/// Journal extension that keeps the last readiness record this build could not
/// decode (for example one written by a later build), byte for byte, before the
/// next evaluation rewrites [`READINESS_KEY`]. Readiness never gates recovery.
pub const READINESS_QUARANTINE_KEY: &str = "slopninja.operation-readiness.unreadable.v1";
/// Readiness entry for rtao's finalized observation, reveal tracking and
/// recovery: its storage reads use compiled descriptors while their shape
/// matches and resolve by name against live metadata otherwise.
pub const OBSERVATION_ID: &str = "rtao.finalized_observation";
/// Retained readiness transitions, oldest dropped first.
const MAX_TRANSITIONS: usize = 64;

fn incompatibility(code: &str, detail: impl Into<String>) -> Incompatibility {
    Incompatibility {
        code: code.into(),
        descriptor: None,
        detail: detail.into(),
        extensions: BTreeMap::new(),
    }
}

/// Signing-boundary check against the runtime the chain serves at this moment.
///
/// It identifies the composed call by name in fresh live metadata, re-encodes
/// its decoded arguments through [`surface::encode`] and requires identical
/// bytes, so the payload type-checks against live types. An operation that the
/// live metadata cannot encode safely is refused for this attempt only; rtao
/// retains the operation and a later attempt re-resolves it.
struct LiveSurfaceGuard {
    inner: Arc<dyn SemanticGuard>,
    genesis: H256,
}

impl SemanticGuard for LiveSurfaceGuard {
    fn check<'a>(
        &'a self,
        client: &'a Client,
        intent: &'a OperationIntent,
        unsigned: &'a UnsignedExtrinsic,
    ) -> rtao_signer::BoxFuture<'a, std::result::Result<(), Incompatibility>> {
        Box::pin(async move {
            if unsigned.genesis_hash() != self.genesis || client.genesis() != self.genesis {
                return Err(incompatibility(
                    "slopninja_chain_identity_mismatch",
                    "The signing chain differs from the journal's chain identity.",
                ));
            }
            let live = client.refresh_runtime().await.map_err(|_| {
                incompatibility(
                    "live_metadata_unavailable",
                    "Live metadata could not be read before signing; the attempt is retried later.",
                )
            })?;
            check_live_call(&live, unsigned.call())?;
            self.inner.check(client, intent, unsigned).await
        })
    }
}

/// Require that composed call bytes belong to the current runtime, decode by
/// name to a known operation, and re-encode identically through live metadata.
pub fn check_live_call(
    live: &RuntimeHandle,
    call: &rtao_runtime::CallBytes,
) -> std::result::Result<(), Incompatibility> {
    if call.ensure_runtime(live).is_err() {
        return Err(incompatibility(
            "runtime_changed_before_signing",
            "The call was composed for an earlier runtime; it is re-resolved against live metadata before any signature.",
        ));
    }
    let (pallet, name, args) = surface::decode_named(live.core(), call.as_bytes())
        .map_err(|deferral| incompatibility(&deferral.code, deferral.detail))?;
    let operations = surface::weight_operations();
    let operation = operations
        .iter()
        .find(|operation| operation.expected.pallet == pallet && operation.expected.call == name)
        .ok_or_else(|| {
            incompatibility(
                "unrecognized_operation",
                "The composed call is not an operation this adapter signs.",
            )
        })?;
    surface::evaluate(live, operation)
        .map_err(|deferral| incompatibility(&deferral.code, deferral.detail))?;
    let values = operation
        .expected
        .args
        .iter()
        .map(|arg| {
            args.iter()
                .find(|(name, _)| name == arg.name)
                .map(|(_, value)| value.clone())
        })
        .collect::<Option<Vec<_>>>()
        .ok_or_else(|| {
            incompatibility(
                "live_type_check_failed",
                "A decoded argument is missing from the composed call.",
            )
        })?;
    let encoded = surface::encode(live.core(), &operation.expected, values)
        .map_err(|deferral| incompatibility(&deferral.code, deferral.detail))?;
    if encoded.bytes != call.as_bytes() {
        return Err(incompatibility(
            "live_type_check_failed",
            "The composed call differs from its live-metadata encoding.",
        ));
    }
    Ok(())
}

fn pause_control(control: &mut JournalControl) {
    control.submissions_paused = true;
    control.allow_obligations = false;
}

/// Informational runtime facts at one evaluation. Never compared for acceptance.
///
/// Readiness records decode tolerantly (no `deny_unknown_fields`): they are
/// advisory and recomputed every block, and an older build must keep working
/// after a newer one added fields.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RuntimeSeen {
    pub spec_version: u32,
    pub transaction_version: u32,
}

/// Live-surface state of one signed operation.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReadinessState {
    /// Encodes against live metadata unchanged.
    Ready,
    /// Encodes against live metadata through recorded safe adaptations.
    Adapted,
    /// No type-correct encoding exists now; this operation alone waits.
    Deferred,
}

/// Durable readiness of one operation, re-evaluated every block.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct OperationReadiness {
    pub state: ReadinessState,
    #[serde(default)]
    pub adaptations: Vec<surface::Adaptation>,
    #[serde(default)]
    pub deferral: Option<surface::Deferral>,
    /// Block at which the current state was first observed.
    pub since_block: u64,
    /// Informational only.
    pub observed: RuntimeSeen,
}

/// One readiness change, retained for operator review.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReadinessTransition {
    pub operation: String,
    pub block: u64,
    pub state: ReadinessState,
    #[serde(default)]
    pub code: Option<String>,
    #[serde(default)]
    pub adaptations: Vec<surface::Adaptation>,
    /// Informational only.
    pub observed: RuntimeSeen,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
struct ReadinessLog {
    schema: u32,
    operations: BTreeMap<String, OperationReadiness>,
    transitions: Vec<ReadinessTransition>,
}

/// Chain facts read by name from live storage that decide whether an
/// operation has a correct encoding. `None` means not read; it defers nothing.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct LiveFacts {
    /// `SubtensorModule::CommitRevealWeightsVersion`, with its live default.
    pub commit_reveal_version: Option<u128>,
}

/// Read [`LiveFacts`] by name at the best head, where rtao's scheduler reads
/// them. A failed or malformed read yields `None` for that fact.
pub async fn read_live_facts(client: &Client) -> LiveFacts {
    let version = async {
        let snapshot = client.at(BlockRef::Best).await.ok()?;
        let value = snapshot
            .query_batch_dyn(
                "SubtensorModule",
                "CommitRevealWeightsVersion",
                vec![vec![]],
            )
            .await
            .ok()?
            .into_iter()
            .next()?;
        value.as_u128()
    };
    LiveFacts {
        commit_reveal_version: version.await,
    }
}

const COMMIT_CALL: &str = "commit_timelocked_mechanism_weights";

/// Evaluate every signed operation, and rtao's finalized observation, against
/// one live runtime. Pure and total: any metadata, including malformed
/// metadata, yields a state per entry.
pub fn evaluate_operations(runtime: &RuntimeHandle) -> BTreeMap<String, OperationReadiness> {
    evaluate_operations_with(runtime, &LiveFacts::default())
}

/// [`evaluate_operations`] with chain facts read by name from live storage.
///
/// rtao composes weight calls and reads its storage through compiled
/// descriptors while their shape matches and by name against live metadata
/// otherwise (D-088, D-089), recorded as adaptations. What is recorded as a
/// deferral, which clears by itself when the live chain changes back or rtao
/// gains support, is what rtao itself refuses:
///
/// - a weight call that neither rtao's compiled descriptor nor its by-name
///   composition can produce (`upstream_compose_refused`);
/// - a storage item or constant rtao reads that neither path accepts
///   (`upstream_storage_refused`); this stops finalized observation and
///   recovery as well as both weight operations;
/// - a stored commit-reveal version that does not fit the call's `u16` tag
///   (`commit_reveal_version_unencodable`).
///
/// Any other stored commit-reveal version is an adaptation of the commit
/// ([`surface::Adaptation::CommitRevealTagChanged`]): rtao follows it under
/// reviewed or proven runtime code and otherwise pauses timelocked commits
/// per attempt, reported as that attempt's outcome.
pub fn evaluate_operations_with(
    runtime: &RuntimeHandle,
    facts: &LiveFacts,
) -> BTreeMap<String, OperationReadiness> {
    let observed = RuntimeSeen {
        spec_version: runtime.spec_version(),
        transaction_version: runtime.transaction_version(),
    };
    let storage = surface::evaluate_upstream_storage(runtime);
    let native = u128::from(rtao_neuron::weights::NATIVE_CR_PAYLOAD_VERSION);
    let commit_version: std::result::Result<Option<surface::Adaptation>, surface::Deferral> =
        match facts.commit_reveal_version {
            None => Ok(None),
            Some(version) if version == native => Ok(None),
            Some(version) => match u16::try_from(version) {
                Ok(stored) => Ok(Some(surface::Adaptation::CommitRevealTagChanged { stored })),
                Err(_) => Err(surface::Deferral::new(
                    "commit_reveal_version_unencodable",
                    format!(
                        "the chain stores commit-reveal version {version}, which no u16 commit tag can carry"
                    ),
                )),
            },
        };
    let state =
        |result: std::result::Result<Vec<surface::Adaptation>, surface::Deferral>| match result {
            Ok(adaptations) if adaptations.is_empty() => OperationReadiness {
                state: ReadinessState::Ready,
                adaptations,
                deferral: None,
                since_block: 0,
                observed,
            },
            Ok(adaptations) => OperationReadiness {
                state: ReadinessState::Adapted,
                adaptations,
                deferral: None,
                since_block: 0,
                observed,
            },
            Err(deferral) => OperationReadiness {
                state: ReadinessState::Deferred,
                adaptations: Vec::new(),
                deferral: Some(deferral),
                since_block: 0,
                observed,
            },
        };
    let mut evaluated: BTreeMap<String, OperationReadiness> = surface::weight_operations()
        .iter()
        .map(|operation| {
            let mut result = surface::evaluate(runtime, operation);
            if let Ok(adaptations) = &mut result {
                if let Err(deferral) = &storage {
                    result = Err(deferral.clone());
                } else if operation.expected.call == COMMIT_CALL {
                    match &commit_version {
                        Ok(Some(adaptation)) => adaptations.push(adaptation.clone()),
                        Ok(None) => {}
                        Err(deferral) => result = Err(deferral.clone()),
                    }
                }
            }
            (operation.expected.id(), state(result))
        })
        .collect();
    evaluated.insert(OBSERVATION_ID.into(), state(storage));
    evaluated
}

/// Redacted readiness for status output: operation names, states and codes.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct ReadinessSummary {
    pub deferred: Vec<DeferredOperation>,
    pub adapted: Vec<String>,
    /// The stored record could not be decoded by this build. It is treated as
    /// absent, kept under [`READINESS_QUARANTINE_KEY`] and rewritten by the
    /// next evaluation; recovery and dispatch never depend on it.
    pub unreadable: bool,
    /// True when any operation is deferred or the record was unreadable;
    /// status shows this as an alert.
    pub alert: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct DeferredOperation {
    pub operation: String,
    pub code: String,
    pub since_block: u64,
}

/// Decode the readiness record. It is advisory and recomputable, so an
/// unreadable one (a later schema, a new adaptation kind, corruption) is
/// treated as absent and returned raw for quarantine; it never fails.
fn read_readiness(
    image: &rtao_client::journal::JournalImage,
) -> (ReadinessLog, Option<serde_json::Value>) {
    let empty = || ReadinessLog {
        schema: 1,
        ..ReadinessLog::default()
    };
    match image.extensions.get(READINESS_KEY) {
        None => (empty(), None),
        Some(value) => match serde_json::from_value::<ReadinessLog>(value.clone()) {
            Ok(log) if log.schema == 1 => (log, None),
            _ => (empty(), Some(value.clone())),
        },
    }
}

fn summarize_read(read: &(ReadinessLog, Option<serde_json::Value>)) -> ReadinessSummary {
    let mut summary = summarize(&read.0);
    if read.1.is_some() {
        summary.unreadable = true;
        summary.alert = true;
    }
    summary
}

fn summarize(log: &ReadinessLog) -> ReadinessSummary {
    let mut summary = ReadinessSummary::default();
    for (operation, readiness) in &log.operations {
        match readiness.state {
            ReadinessState::Deferred => summary.deferred.push(DeferredOperation {
                operation: operation.clone(),
                code: readiness
                    .deferral
                    .as_ref()
                    .map_or_else(String::new, |deferral| deferral.code.clone()),
                since_block: readiness.since_block,
            }),
            ReadinessState::Adapted => summary.adapted.push(operation.clone()),
            ReadinessState::Ready => {}
        }
    }
    summary.alert = !summary.deferred.is_empty();
    summary
}

/// Identity of the only validator permitted to use one journal file.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChainIdentity {
    pub genesis: H256,
    pub netuid: NetUid,
    pub hotkey: AccountId,
}

/// Immutable application meaning, independent of block height and runtime version.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WeightWork {
    /// Hash of the frozen evaluation manifest, not an operational build hash.
    pub manifest_hash: [u8; 32],
    pub cohort: String,
    pub revision: u64,
    pub mechanism: MechId,
    /// `SubnetEpochIndex` of a per-tempo re-commit of this target (plan
    /// decision 12). `None` is the target's first commit; it is omitted from
    /// the serialized correlation, so work enqueued before re-commits existed
    /// keeps its exact retained intent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tempo: Option<u64>,
    /// `reward-v1`: the active calculation target's digest. Each tempo's
    /// identity is then the protocol's `WeightOperationV2` (chain, mechanism,
    /// validator, target and epoch), which the validator issues only under a
    /// checked authorization. Absent for `testnet-reward-v0` work, whose
    /// retained intents keep their exact correlation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub v2_target: Option<[u8; 32]>,
}

impl WeightWork {
    /// The row is excluded so changed input conflicts with the original operation.
    /// Delegates to the shared protocol identity, so native and Workers
    /// validators derive the same operation ID. A per-tempo re-commit has the
    /// protocol's tempo identity over the target's first-commit identity.
    pub fn operation_id(&self, identity: &ChainIdentity) -> Result<String> {
        MechId::new(self.mechanism.0)?;
        if let Some(target) = self.v2_target {
            let epoch = self.tempo.context("a reward-v1 operation names its tempo")?;
            return Ok(slop_ninja_protocol::WeightOperationV2 {
                genesis: slop_ninja_protocol::Digest(identity.genesis.0),
                netuid: identity.netuid.0,
                mechanism: self.mechanism.0,
                hotkey: slop_ninja_protocol::AccountId(identity.hotkey.0),
                target: slop_ninja_protocol::Digest(target),
                epoch,
            }
            .id());
        }
        let base = slop_ninja_protocol::weight_operation_id(
            &identity.genesis.0,
            identity.netuid.0,
            &identity.hotkey.0,
            &self.manifest_hash,
            &self.cohort,
            self.revision,
            self.mechanism.0,
        )
        .context("invalid cohort identity")?;
        match self.tempo {
            None => Ok(base),
            Some(epoch) => slop_ninja_protocol::weight_tempo_operation_id(&base, epoch)
                .context("invalid tempo identity"),
        }
    }

    /// The same target's re-commit for subnet epoch `epoch`.
    pub fn for_tempo(&self, epoch: u64) -> Self {
        Self {
            tempo: Some(epoch),
            ..self.clone()
        }
    }

    /// Whether `other` re-commits (or is) the same target: every field but
    /// the tempo is equal.
    pub fn same_target(&self, other: &Self) -> bool {
        self.manifest_hash == other.manifest_hash
            && self.cohort == other.cohort
            && self.revision == other.revision
            && self.mechanism == other.mechanism
            && self.v2_target == other.v2_target
    }
}

/// A retained operation of one target, for per-tempo scheduling. Redacted: no
/// row, signed bytes, nonce or commit material.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct TargetOperation {
    pub id: String,
    /// `None` for the target's first commit.
    pub tempo: Option<u64>,
    /// Row hash recorded in the correlation at enqueue.
    pub row_hash: String,
    /// rtao handed the operation back after its finalized success; only then
    /// can another operation for the mechanism be queued.
    pub handed_off: bool,
    pub status: OperationStatus,
}

/// A retained operation of one mechanism that rtao has not handed back, of
/// any target. Redacted like [`TargetOperation`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct PendingOperation {
    pub id: String,
    /// The correlated work, when the record carries a readable correlation.
    pub work: Option<WeightWork>,
    pub row_hash: Option<String>,
    pub status: OperationStatus,
}

impl PendingOperation {
    /// Never landed and can never land: no attempt awaits classification
    /// and none finalized successfully (no attempt at all, or only attempts
    /// finalized recovery classified as expired unexecuted or failed).
    pub fn retirable(&self) -> bool {
        self.status.unresolved == 0 && self.status.finalized_success_block.is_none()
    }
}

/// Journal extension recording why an operation was retired.
pub const RETIRED_KEY: &str = "slop_ninja_retired";

/// The rtao commit every crate here pins (a test checks each manifest).
/// When it changes, set [`PREVIOUS_RTAO_PIN`] to the old value, re-derive
/// the field lists below from that commit's source and update the copied
/// structures in the rollback test to match.
pub const RTAO_PIN: &str = "0c12816009baa65eec3135bac9a9dbcc8eaba77d";

/// The rtao pin before the current one ([`RTAO_PIN`]), whose strict journal
/// decoding [`JournalControls::rollback_blockers`] checks a journal against.
pub const PREVIOUS_RTAO_PIN: &str = "b5553a76ff67f0f54350472ef0306f225e8a4c44";

// Fields of the structures rtao `b5553a76` decodes with
// `deny_unknown_fields` from a journal, from its source at that commit. A
// test decodes journals with those structures, copied from that source, and
// requires them to agree with `rollback_blockers`.
const PREVIOUS_WORK_FIELDS: &[&str] = &["netuid", "mechanism", "input", "version_key"];
const PREVIOUS_MATERIAL_FIELDS: &[&str] = &["row", "commit", "preparation"];
const PREVIOUS_PREPARATION_FIELDS: &[&str] = &["prepared_block", "uid", "reveal_period", "schedule_fields"];
const PREVIOUS_WATCHER_FIELDS: &[&str] = &["pending", "unlocated", "outcomes", "last_observed_block"];
const PREVIOUS_PENDING_FIELDS: &[&str] = &[
    "netuid",
    "mechanism",
    "hotkey",
    "uid",
    "uids",
    "version_key",
    "commit_epoch",
    "reveal_epoch",
    "predicted_first_block",
    "round",
    "ciphertext",
    "inclusion_block",
    "first_check",
    "first_checked",
    "first_reveal_hash",
    "last_round",
    "notices",
    "warned_permit",
    "ambiguous_reveal",
];
const PREVIOUS_UNLOCATED_FIELDS: &[&str] =
    &["netuid", "mechanism", "hotkey", "row", "version_key", "state", "commit", "report"];

/// A field a build at [`PREVIOUS_RTAO_PIN`] refuses to decode. Redacted: a
/// field path and an operation ID only, no values.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct RollbackBlocker {
    /// The pending operation holding the field; `None` for the weight checkpoint.
    pub operation_id: Option<String>,
    /// Path of the refused field, such as `effects.owner_rows` or
    /// `checkpoint.reveals.revealed_code`.
    pub field: String,
    /// The operation never landed and no attempt awaits classification, so
    /// [`JournalControls::retire_for_rollback`] can retire it.
    pub retirable: bool,
}

/// This adapter has no durable shield carrier/inner recovery path.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Shielding {
    /// Explicitly permit ordinary dispatch if current chain policy also permits it.
    Unshielded,
    /// Refuse construction rather than silently downgrade required shielding.
    Required,
    /// Automatic selection could require shielding; it also fails closed here.
    Automatic,
}

/// Caller-selected semantics and cadence; none is inferred from live chain storage.
pub struct SchedulerConfig {
    pub identity: ChainIdentity,
    pub semantic_contract: String,
    pub weight_version: u64,
    pub cadence: Option<WeightCadence>,
    pub shielding: Shielding,
    /// Each weight transaction's admission window (signer resolution, nonce
    /// lease, preparation, signing and the pool's acknowledgment), passed to
    /// rtao's `WeightScheduler::with_submit_deadline` (D-097). `None` keeps
    /// rtao's default, `DEFAULT_WEIGHT_SUBMIT_DEADLINE`. The caller sizes it
    /// from the request budget of the transport that signs; rate-limiter
    /// waits count toward it.
    pub submit_deadline: Option<std::time::Duration>,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ApplicationIdentity {
    schema: u32,
    chain: ChainIdentity,
    pause_epoch: u64,
}

/// Application context retained in an operation's correlation ID for its guard.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WeightCorrelation {
    pub schema: u32,
    pub work: WeightWork,
    pub row_hash: String,
}

/// Result of retaining an immutable logical row; neither variant means inclusion.
#[derive(Debug, PartialEq, Eq)]
pub enum Enqueued {
    Created(String),
    Existing(String),
}

/// Deliberately excludes addresses, rows, signed bytes, ciphertext and caller text.
#[derive(Debug, Serialize)]
pub struct JournalSummary {
    pub schema: u32,
    pub generation: u64,
    pub paused: bool,
    pub operations: usize,
    pub attempts: usize,
    pub unresolved_attempts: usize,
    pub finalized_failures: usize,
    pub semantic_refusals: usize,
    /// Live-surface readiness of signed operations; deferral raises the alert.
    pub readiness: ReadinessSummary,
}

/// Redacted per-operation state. Neither field proves business settlement.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct OperationStatus {
    pub attempts: usize,
    /// Persisted before dispatch; a crash or lost response may be on either side.
    pub unresolved: usize,
    pub expired: usize,
    pub finalized_failures: usize,
    /// Finalized inclusion height of a successful attempt.
    pub finalized_success_block: Option<u64>,
    /// Last semantic refusal code recorded by the signing guard.
    pub semantic_refusal: Option<String>,
}

/// Shared handle for offline inspection and persistent pause, without a signer.
#[derive(Clone)]
pub struct JournalControls {
    journal: Arc<OperationJournal>,
    identity: ChainIdentity,
}

impl JournalControls {
    /// Open an existing Slop Ninja journal. A misspelled path never initializes one.
    /// The native lifetime lock refuses opening while another process owns it.
    pub fn open_existing(path: impl AsRef<Path>) -> Result<Self> {
        ensure!(path.as_ref().is_file(), "journal file does not exist");
        let journal = OperationJournal::open(path)?;
        let image = journal.snapshot()?;
        let saved = image
            .extensions
            .get(APPLICATION_KEY)
            .context("journal has no application identity")?;
        let identity: ApplicationIdentity = serde_json::from_value(saved.clone())?;
        ensure!(
            identity.schema == 1,
            "unsupported application journal schema"
        );
        Ok(Self {
            journal,
            identity: identity.chain,
        })
    }

    pub fn inspect(&self) -> Result<JournalSummary> {
        let image = self.journal.snapshot()?;
        let mut result = JournalSummary {
            schema: image.schema_version,
            generation: image.revision,
            paused: image.control.submissions_paused,
            operations: image.operations.len(),
            attempts: 0,
            unresolved_attempts: 0,
            finalized_failures: 0,
            semantic_refusals: 0,
            readiness: summarize_read(&read_readiness(&image)),
        };
        for operation in image.operations.values() {
            result.attempts += operation.attempts.len();
            result.semantic_refusals += usize::from(operation.incompatibility.is_some());
            for attempt in &operation.attempts {
                match &attempt.outcome {
                    AttemptOutcome::Unresolved => result.unresolved_attempts += 1,
                    AttemptOutcome::Finalized { success: false, .. } => {
                        result.finalized_failures += 1
                    }
                    _ => {}
                }
            }
        }
        Ok(result)
    }

    /// Stops new work and dispatch. Observation and native chain processing continue.
    pub fn pause(&self) -> Result<()> {
        self.journal.update(|image| {
            advance_pause_epoch(image)?;
            pause_control(&mut image.control);
            Ok(())
        })?;
        Ok(())
    }

    pub fn identity(&self) -> &ChainIdentity {
        &self.identity
    }

    /// Redacted state of one operation: attempt counts and finalized
    /// inclusion, without signed bytes, nonces or commit material.
    pub fn operation(&self, id: &str) -> Result<Option<OperationStatus>> {
        let image = self.journal.snapshot()?;
        Ok(image.operations.get(id).map(|record| {
            let mut status = OperationStatus {
                attempts: record.attempts.len(),
                ..OperationStatus::default()
            };
            for attempt in &record.attempts {
                match &attempt.outcome {
                    AttemptOutcome::Unresolved => status.unresolved += 1,
                    AttemptOutcome::ExpiredUnexecuted => status.expired += 1,
                    AttemptOutcome::Finalized {
                        number,
                        success: true,
                        ..
                    } => status.finalized_success_block = Some(*number),
                    AttemptOutcome::Finalized { success: false, .. } => {
                        status.finalized_failures += 1
                    }
                }
            }
            status.semantic_refusal = record.incompatibility.as_ref().map(|i| i.code.clone());
            status
        }))
    }

    /// Every retained operation of `work`'s target (its first commit and each
    /// per-tempo re-commit), ordered first commit first, then by tempo.
    /// Operations of other targets, and records without a readable weight
    /// correlation, are skipped.
    pub fn target_operations(&self, work: &WeightWork) -> Result<Vec<TargetOperation>> {
        let image = self.journal.snapshot()?;
        let mut found: Vec<TargetOperation> = image
            .operations
            .values()
            .filter_map(|record| {
                let correlation: WeightCorrelation =
                    serde_json::from_str(record.intent.correlation_id.as_deref()?).ok()?;
                if !work.same_target(&correlation.work) {
                    return None;
                }
                Some((record.intent.id.clone(), correlation, record))
            })
            .map(|(id, correlation, record)| {
                let status = self.operation(&id)?.unwrap_or_default();
                Ok(TargetOperation {
                    tempo: correlation.work.tempo,
                    row_hash: correlation.row_hash,
                    handed_off: record.extensions.get("scheduler_handed_off")
                        == Some(&serde_json::Value::Bool(true)),
                    status,
                    id,
                })
            })
            .collect::<Result<_>>()?;
        found.sort_by_key(|operation| operation.tempo.map_or(0, |epoch| epoch.saturating_add(1)));
        Ok(found)
    }

    /// Every retained operation of `mechanism` that rtao has not handed back,
    /// whatever its target or tempo. rtao queues at most one per mechanism.
    pub fn pending_operations(&self, mechanism: MechId) -> Result<Vec<PendingOperation>> {
        let image = self.journal.snapshot()?;
        let mut out = Vec::new();
        for record in image.operations.values() {
            if record.extensions.get("scheduler_handed_off") == Some(&serde_json::Value::Bool(true)) {
                continue;
            }
            let effects_mechanism = record.intent.effects.get("mechanism").and_then(serde_json::Value::as_u64);
            if effects_mechanism != Some(u64::from(mechanism.0)) {
                continue;
            }
            let correlation: Option<WeightCorrelation> = record
                .intent
                .correlation_id
                .as_deref()
                .and_then(|c| serde_json::from_str(c).ok());
            out.push(PendingOperation {
                id: record.intent.id.clone(),
                row_hash: correlation.as_ref().map(|c| c.row_hash.clone()),
                work: correlation.map(|c| c.work),
                status: self.operation(&record.intent.id)?.unwrap_or_default(),
            });
        }
        Ok(out)
    }

    /// Retires a pending operation that never landed and can never land
    /// ([`PendingOperation::retirable`]), so another operation of its
    /// mechanism can be queued: a successor target, the tempo after a missed
    /// one, or a row re-derived from a fresh snapshot. The retirement is
    /// journaled in one update: rtao's own hand-off mark, which its queue
    /// reads, and [`RETIRED_KEY`] with `reason`. Nothing was signed, or every
    /// signed attempt expired or failed, so no commitment is lost or doubled.
    /// Returns false, writing nothing, when the operation is absent, already
    /// handed back, or not retirable.
    pub fn retire_superseded(&self, id: &str, reason: &str) -> Result<bool> {
        ensure!(
            !reason.is_empty() && reason.len() <= 64 && reason.bytes().all(|b| b.is_ascii_lowercase() || b == b'_'),
            "retirement reason"
        );
        let mut retired = false;
        self.journal.update(|image| {
            let Some(record) = image.operations.get_mut(id) else {
                return Ok(());
            };
            if record.extensions.get("scheduler_handed_off") == Some(&serde_json::Value::Bool(true)) {
                return Ok(());
            }
            let unresolved = record.attempts.iter().any(|a| matches!(a.outcome, AttemptOutcome::Unresolved));
            let landed = record
                .attempts
                .iter()
                .any(|a| matches!(a.outcome, AttemptOutcome::Finalized { success: true, .. }));
            if unresolved || landed {
                return Ok(());
            }
            record
                .extensions
                .insert("scheduler_handed_off".into(), serde_json::Value::Bool(true));
            record.extensions.insert(
                RETIRED_KEY.into(),
                serde_json::json!({ "schema": 1, "reason": reason, "attempts": record.attempts.len() }),
            );
            retired = true;
            Ok(())
        })?;
        Ok(retired)
    }

    /// The row hash recorded in a retained operation's correlation and
    /// whether rtao has handed it back, if the journal retains it.
    pub fn retained_row(&self, id: &str) -> Result<Option<(String, bool)>> {
        let image = self.journal.snapshot()?;
        Ok(image.operations.get(id).and_then(|record| {
            let correlation: WeightCorrelation = serde_json::from_str(record.intent.correlation_id.as_deref()?).ok()?;
            let handed_off =
                record.extensions.get("scheduler_handed_off") == Some(&serde_json::Value::Bool(true));
            Some((correlation.row_hash, handed_off))
        }))
    }

    /// The owner-row policy a retained operation was queued under, read from
    /// its journaled effects (absent `owner_rows` is `Hold`), if the journal
    /// retains it. An unreadable value is an error, never a default.
    pub fn owner_row_policy(&self, id: &str) -> Result<Option<OwnerRowPolicy>> {
        let image = self.journal.snapshot()?;
        let Some(record) = image.operations.get(id) else {
            return Ok(None);
        };
        match record.intent.effects.get("owner_rows") {
            None => Ok(Some(OwnerRowPolicy::Hold)),
            Some(value) => Ok(Some(
                serde_json::from_value(value.clone()).context("retained owner-row policy is unreadable")?,
            )),
        }
    }

    /// Fields in this journal that a build at [`PREVIOUS_RTAO_PIN`] refuses
    /// to decode, so that build cannot open the journal's weight scope. That
    /// rtao decodes these structures with `deny_unknown_fields`: the effects
    /// (`Work`) and attempt material (`Material`, its `RevealPreparation`) of
    /// every weight record it has not handed back, and the weight checkpoint
    /// (`Checkpoint`, `RevealWatcher`, `Pending`, `Unlocated`). The current
    /// pin adds `owner_rows` to `Submit` work, `call_encoder` to material an
    /// application encoder composed, `followed_cr` to a preparation and a
    /// pending reveal that followed a non-native commit-reveal tag, and
    /// `revealed_code` to the watcher once a reveal completed, or was
    /// missed, under one (retained evidence, never cleared).
    /// Handed-back records are never decoded there, so finished or retired
    /// work blocks nothing; the slot's `unsigned_retry` is accepted there.
    /// Empty means a build at that pin can open the journal.
    pub fn rollback_blockers(&self) -> Result<Vec<RollbackBlocker>> {
        let image = self.journal.snapshot()?;
        let mut found = Vec::new();
        let mut unknown = |operation_id: Option<&str>, path: &str, value: &serde_json::Value, known: &[&str], retirable: bool| {
            if let Some(object) = value.as_object() {
                for key in object.keys().filter(|key| !known.contains(&key.as_str())) {
                    found.push(RollbackBlocker {
                        operation_id: operation_id.map(str::to_owned),
                        field: format!("{path}{key}"),
                        retirable,
                    });
                }
            }
        };
        for record in image.operations.values() {
            if !record.intent.scope.starts_with("weights:")
                || record.extensions.get("scheduler_handed_off") == Some(&serde_json::Value::Bool(true))
            {
                continue;
            }
            let retirable = !record.attempts.iter().any(|a| {
                matches!(a.outcome, AttemptOutcome::Unresolved | AttemptOutcome::Finalized { success: true, .. })
            });
            let id = Some(record.intent.id.as_str());
            unknown(id, "effects.", &record.intent.effects, PREVIOUS_WORK_FIELDS, retirable);
            for attempt in &record.attempts {
                unknown(id, "attempt.material.", &attempt.material, PREVIOUS_MATERIAL_FIELDS, retirable);
                if let Some(preparation) = attempt.material.get("preparation") {
                    unknown(id, "attempt.material.preparation.", preparation, PREVIOUS_PREPARATION_FIELDS, retirable);
                }
            }
        }
        for (scope, checkpoint) in &image.checkpoints {
            if !scope.starts_with("weights:") {
                continue;
            }
            unknown(None, "checkpoint.", checkpoint, &["schema", "slots", "reveals"], false);
            let Some(reveals) = checkpoint.get("reveals") else { continue };
            unknown(None, "checkpoint.reveals.", reveals, PREVIOUS_WATCHER_FIELDS, false);
            for pending in reveals.get("pending").and_then(serde_json::Value::as_array).into_iter().flatten() {
                unknown(None, "checkpoint.reveals.pending.", pending, PREVIOUS_PENDING_FIELDS, false);
            }
            for unlocated in reveals.get("unlocated").and_then(serde_json::Value::as_array).into_iter().flatten() {
                unknown(None, "checkpoint.reveals.unlocated.", unlocated, PREVIOUS_UNLOCATED_FIELDS, false);
                if let Some(state) = unlocated.get("state") {
                    unknown(None, "checkpoint.reveals.unlocated.state.", state, PREVIOUS_PREPARATION_FIELDS, false);
                }
            }
        }
        found.sort_by(|a, b| (&a.operation_id, &a.field).cmp(&(&b.operation_id, &b.field)));
        found.dedup();
        Ok(found)
    }

    /// Retires every weight operation that blocks a rollback
    /// ([`Self::rollback_blockers`]) and can be retired
    /// ([`Self::retire_superseded`], reason `rollback_prepared`): queued
    /// work that never signed, such as the next tempo's `Submit` burn row
    /// waiting for its epoch's offset. Run only with the validator stopped,
    /// which the journal's lifetime lock enforces. A validator of this build
    /// started again queues that tempo's work afresh under the next label,
    /// since a retired label is never joined: the label moves one epoch
    /// ahead of the epoch it commits in from then on, so the last epoch of
    /// an authorization lease is deferred (`authorization_refused`) one tempo
    /// early. Nothing is lost or committed twice. Returns the retired IDs.
    /// Checkpoint fields, and work with an attempt awaiting recovery or a
    /// landed commit not yet handed back, are left: they settle only by
    /// running this build.
    pub fn retire_for_rollback(&self) -> Result<Vec<String>> {
        let mut ids: Vec<String> = self
            .rollback_blockers()?
            .into_iter()
            .filter(|blocker| blocker.retirable)
            .filter_map(|blocker| blocker.operation_id)
            .collect();
        ids.dedup();
        let mut retired = Vec::new();
        for id in ids {
            if self.retire_superseded(&id, "rollback_prepared")? {
                retired.push(id);
            }
        }
        Ok(retired)
    }

    /// Whether the operation was retired, and why.
    pub fn retired(&self, id: &str) -> Result<Option<String>> {
        let image = self.journal.snapshot()?;
        Ok(image
            .operations
            .get(id)
            .and_then(|r| r.extensions.get(RETIRED_KEY))
            .and_then(|v| v.get("reason"))
            .and_then(serde_json::Value::as_str)
            .map(str::to_owned))
    }

    /// Persisted per-operation readiness, as last evaluated against live metadata.
    pub fn readiness(&self) -> Result<BTreeMap<String, OperationReadiness>> {
        Ok(read_readiness(&self.journal.snapshot()?).0.operations)
    }

    /// Retained readiness transitions, including every recorded adaptation.
    pub fn readiness_transitions(&self) -> Result<Vec<ReadinessTransition>> {
        Ok(read_readiness(&self.journal.snapshot()?).0.transitions)
    }

    /// Record a live-surface evaluation. The journal is written only when an
    /// operation's state, deferral code or adaptations change, so the record
    /// survives restarts without a write per block. Evaluation never pauses or
    /// refuses other work; it only describes which operation must wait.
    pub fn record_readiness(
        &self,
        block: u64,
        evaluated: BTreeMap<String, OperationReadiness>,
    ) -> Result<ReadinessSummary> {
        let (current, unreadable) = read_readiness(&self.journal.snapshot()?);
        let changed = |name: &String, next: &OperationReadiness| {
            current.operations.get(name).is_none_or(|previous| {
                previous.state != next.state
                    || previous.adaptations != next.adaptations
                    || previous.deferral.as_ref().map(|d| &d.code)
                        != next.deferral.as_ref().map(|d| &d.code)
            })
        };
        if unreadable.is_none()
            && evaluated.len() == current.operations.len()
            && !evaluated.iter().any(|(name, next)| changed(name, next))
        {
            return Ok(summarize(&current));
        }
        let mut written = None;
        self.journal.update(|image| {
            let (mut log, unreadable) = read_readiness(image);
            if let Some(raw) = unreadable {
                image
                    .extensions
                    .insert(READINESS_QUARANTINE_KEY.into(), raw);
            }
            let mut next_operations = BTreeMap::new();
            for (name, mut next) in evaluated.clone() {
                match log.operations.get(&name) {
                    Some(previous)
                        if previous.state == next.state
                            && previous.adaptations == next.adaptations
                            && previous.deferral.as_ref().map(|d| &d.code)
                                == next.deferral.as_ref().map(|d| &d.code) =>
                    {
                        next.since_block = previous.since_block;
                    }
                    _ => {
                        next.since_block = block;
                        log.transitions.push(ReadinessTransition {
                            operation: name.clone(),
                            block,
                            state: next.state,
                            code: next.deferral.as_ref().map(|d| d.code.clone()),
                            adaptations: next.adaptations.clone(),
                            observed: next.observed,
                        });
                    }
                }
                next_operations.insert(name, next);
            }
            log.operations = next_operations;
            let excess = log.transitions.len().saturating_sub(MAX_TRANSITIONS);
            log.transitions.drain(..excess);
            image.extensions.insert(
                READINESS_KEY.into(),
                serde_json::to_value(&log).map_err(|_| JournalError::Identity)?,
            );
            written = Some(summarize(&log));
            Ok(())
        })?;
        written.context("readiness update did not run")
    }
}

/// Bounded observation status; a successful RPC alone never certifies recovery.
#[derive(Debug, Serialize)]
pub struct RecoveryProgress {
    pub finalized_height: u64,
    pub reveal_history_caught_up: bool,
    pub outstanding_reveals: Vec<OutstandingReveal>,
    pub unresolved_attempts: usize,
    pub finalized_failures: usize,
    pub semantic_contract_mismatches: usize,
    pub paused_during_recovery: bool,
    pub ready_to_resume: bool,
}

/// Public reveal diagnostics omit commitment ciphertext and the signing account.
#[derive(Debug, Serialize)]
pub struct OutstandingReveal {
    pub mechanism: u8,
    pub inclusion_block: u64,
    pub reveal_epoch: Option<u64>,
    pub attribution_unresolved: bool,
}

/// One native scheduler owns one shared durable journal for its whole lifetime.
pub struct NativeWeightAdapter {
    scheduler: WeightScheduler,
    controls: JournalControls,
    config: SchedulerConfig,
    recovered: bool,
    recovered_generation: Option<u64>,
}

impl NativeWeightAdapter {
    /// A semantic guard is mandatory. Implement it against the application's
    /// frozen manifest and current chain meaning; there is no permissive default.
    ///
    /// Every open persists pause before exposing dispatch. Existing work survives
    /// compatible builds; a guard with a different contract cannot sign old work.
    pub fn open(
        path: impl AsRef<Path>,
        config: SchedulerConfig,
        semantic_guard: Arc<dyn SemanticGuard>,
        clock: Arc<dyn Clock>,
        clock_guard: ClockGuard,
    ) -> Result<Self> {
        ensure!(
            config.shielding == Shielding::Unshielded,
            "durable shielded weights require an upstream carrier/inner recovery adapter"
        );
        ensure!(
            !config.semantic_contract.trim().is_empty() && config.semantic_contract.len() <= 256,
            "a nonempty semantic contract is required"
        );
        let journal = OperationJournal::open(path)?;
        let identity = config.identity.clone();
        journal.update(|image| {
            if let Some(saved) = image.extensions.get(APPLICATION_KEY) {
                let saved: ApplicationIdentity =
                    serde_json::from_value(saved.clone()).map_err(|_| JournalError::Identity)?;
                if saved.schema != 1 || saved.chain != identity {
                    return Err(JournalError::Identity);
                }
            } else {
                if !image.operations.is_empty()
                    || !image.checkpoints.is_empty()
                    || !image.extensions.is_empty()
                {
                    return Err(JournalError::Identity);
                }
                image.extensions.insert(
                    APPLICATION_KEY.into(),
                    serde_json::to_value(ApplicationIdentity {
                        schema: 1,
                        chain: identity.clone(),
                        pause_epoch: 0,
                    })
                    .map_err(|_| JournalError::Identity)?,
                );
            }
            advance_pause_epoch(image)?;
            pause_control(&mut image.control);
            Ok(())
        })?;
        let scheduler = WeightScheduler::new(
            WeightPolicy {
                version_key: VersionKey::Fixed(config.weight_version),
                cadence: config.cadence,
            },
            clock,
            clock_guard,
        )?
        .with_submit_deadline(
            config
                .submit_deadline
                .unwrap_or(rtao_neuron::weights::DEFAULT_WEIGHT_SUBMIT_DEADLINE),
        )?
        .with_journal(
            journal.clone(),
            identity.genesis,
            identity.netuid,
            identity.hotkey,
        )?
        .with_semantic_guard(
            config.semantic_contract.clone(),
            Arc::new(LiveSurfaceGuard {
                inner: semantic_guard,
                genesis: identity.genesis,
            }),
        );
        Ok(Self {
            scheduler,
            controls: JournalControls { journal, identity },
            config,
            recovered: false,
            recovered_generation: None,
        })
    }

    /// Offline tests only: clear the persisted pause so storage-level enqueue
    /// runs without a chain. Recovery stays incomplete, so `resume` and
    /// `dispatch_finalized` still refuse and no signer can be attached.
    #[cfg(feature = "offline-test-hooks")]
    pub fn unpause_storage_for_offline_tests(&self) -> Result<()> {
        self.controls.journal.control(JournalControl::default())?;
        Ok(())
    }

    /// Whether this process completed finalized recovery and nothing since
    /// requires another round before dispatch.
    pub fn is_recovered(&self) -> bool {
        self.recovered
    }

    /// Clone the control handle, preserving the same Arc and native lifetime lock.
    pub fn controls(&self) -> JournalControls {
        self.controls.clone()
    }

    /// Inspect rtao's last per-mechanism result. An outer dispatch Ok can contain
    /// a scheduled wait, semantic refusal, runtime-surface pause or preparation
    /// failure. Callers must inspect these outcomes rather than infer inclusion.
    /// These are sensitive internal diagnostics and must not be public status.
    pub fn last_attempt(&self, mechanism: MechId) -> Option<&rtao_neuron::weights::WeightAttempt> {
        self.scheduler.last_attempt(mechanism)
    }

    /// Retained reveal observations, including bounded completed diagnostics.
    /// The watcher retains sensitive material. Never serialize it into public
    /// status; use the redacted recovery summary for public diagnostics.
    pub fn reveals(&self) -> &rtao_neuron::weights::RevealWatcher {
        self.scheduler.reveals()
    }

    /// Persist already authenticated application output. Authentication and cohort
    /// evidence remain the caller's responsibility and are checked by its guard.
    /// A retry of completed work stays idempotent even with later work pending.
    /// The work is prepared under [`OwnerRowPolicy::Hold`]; see
    /// [`Self::enqueue_with_owner_rows`].
    pub fn enqueue(&mut self, work: &WeightWork, row: BTreeMap<Uid, f64>) -> Result<Enqueued> {
        self.enqueue_with_owner_rows(work, row, OwnerRowPolicy::Hold)
    }

    /// [`Self::enqueue`] under an explicit owner-row policy, journaled with the
    /// work through rtao's `enqueue_with_owner_rows`. `Submit` work records
    /// `"owner_rows":"submit"` in its immutable effects; `Hold` work keeps the
    /// exact bytes earlier builds wrote. Restart, recovery and the re-prepare
    /// after a rejected commitment use the recorded policy. Retaining the same
    /// ID under another policy is refused like changed input.
    ///
    /// Choose the policy with [`OwnerRowPolicy::holds`] first: rtao journals
    /// work before its guard runs, so a row the policy holds would stay
    /// pending and block the mechanism.
    pub fn enqueue_with_owner_rows(
        &mut self,
        work: &WeightWork,
        row: BTreeMap<Uid, f64>,
        owner_rows: OwnerRowPolicy,
    ) -> Result<Enqueued> {
        let id = work.operation_id(&self.config.identity)?;
        let row = canonical_row(row)?;
        let correlation = serde_json::to_string(&WeightCorrelation {
            schema: 1,
            work: work.clone(),
            row_hash: row_hash(&row),
        })?;
        let input = WeightsInput::ByUid(row);
        let identity = &self.config.identity;
        // This projection deliberately follows rtao's journaled `Work` at the
        // exact pinned revision: `owner_rows` is omitted for `Hold`.
        let mut effects = serde_json::json!({ "netuid": identity.netuid, "mechanism": work.mechanism, "input": input, "version_key": self.config.weight_version });
        if owner_rows != OwnerRowPolicy::Hold {
            effects["owner_rows"] = serde_json::to_value(owner_rows)?;
        }
        let expected = OperationIntent {
            id: id.clone(),
            correlation_id: Some(correlation.clone()),
            genesis: identity.genesis,
            account: identity.hotkey,
            scope: format!(
                "weights:{}:{}:{}",
                hex::encode(identity.genesis.0),
                identity.netuid,
                identity.hotkey
            ),
            effects,
            obligation: false,
            semantic_policy: Some(self.config.semantic_contract.clone()),
            extensions: BTreeMap::new(),
        };
        if let Some(existing) = self.controls.journal.snapshot()?.operations.get(&id) {
            ensure!(
                existing.intent == expected,
                "operation identity, immutable row or owner-row policy conflicts with retained work"
            );
            return Ok(Enqueued::Existing(id));
        }
        self.scheduler.enqueue_with_owner_rows(
            id.clone(),
            Some(correlation),
            work.mechanism,
            input,
            owner_rows,
        )?;
        Ok(Enqueued::Created(id))
    }

    /// Observe finalized history without private keys or author RPC calls.
    /// This entry point persists pause first; successful recovery still requires
    /// an explicit resume. Upstream recovery during dispatch remains separate.
    /// Each call is bounded by rtao to 128 transaction blocks per active operation
    /// and 256 reveal blocks. Repeated calls continue durable cursors.
    pub async fn recover_finalized(
        &mut self,
        client: &Client,
        finalized: &BlockHeader,
        metrics: &Metrics,
    ) -> Result<RecoveryProgress> {
        let pause_before = self.begin_recovery()?;
        ensure!(
            client.genesis() == self.config.identity.genesis,
            "recovery chain identity differs"
        );
        self.scheduler
            .recover_finalized(client, finalized, metrics)
            .await?;
        let summary = self.controls.inspect()?;
        let caught_up = self.scheduler.reveals().caught_up(finalized.number);
        let outstanding_reveals: Vec<_> = self
            .scheduler
            .reveals()
            .outstanding()
            .into_iter()
            .map(|reveal| OutstandingReveal {
                mechanism: reveal.mechanism.0,
                inclusion_block: reveal.inclusion_block,
                reveal_epoch: reveal.reveal_epoch,
                attribution_unresolved: reveal.attribution_unresolved,
            })
            .collect();
        let after = self.controls.journal.snapshot()?;
        let paused_during_recovery = pause_before != pause_epoch(&after)?;
        let semantic_contract_mismatches = after
            .operations
            .values()
            .filter(|record| {
                record.extensions.get("scheduler_handed_off")
                    != Some(&serde_json::Value::Bool(true))
                    && record.intent.semantic_policy.as_deref()
                        != Some(self.config.semantic_contract.as_str())
            })
            .count();
        self.recovered = !paused_during_recovery
            && caught_up
            && summary.unresolved_attempts == 0
            && summary.finalized_failures == 0
            && semantic_contract_mismatches == 0
            && outstanding_reveals
                .iter()
                .all(|reveal| reveal.reveal_epoch.is_some() && !reveal.attribution_unresolved);
        if self.recovered {
            self.recovered_generation = Some(summary.generation);
        }
        Ok(RecoveryProgress {
            finalized_height: finalized.number,
            reveal_history_caught_up: caught_up,
            outstanding_reveals,
            unresolved_attempts: summary.unresolved_attempts,
            finalized_failures: summary.finalized_failures,
            semantic_contract_mismatches,
            paused_during_recovery,
            ready_to_resume: self.recovered,
        })
    }

    fn begin_recovery(&mut self) -> Result<u64> {
        self.recovered = false;
        self.recovered_generation = None;
        self.controls.pause()?;
        Ok(pause_epoch(&self.controls.journal.snapshot()?)?)
    }

    /// Explicitly allow new work after this process has reconciled finalized history.
    /// Dispatch still performs fresh upstream runtime, semantic and cadence checks.
    pub fn resume_after_recovery(&mut self) -> Result<()> {
        ensure!(
            self.recovered,
            "complete finalized recovery in this process before resume"
        );
        let summary = self.controls.inspect()?;
        ensure!(
            self.recovered_generation == Some(summary.generation),
            "journal changed since recovery; reconcile again before resume"
        );
        ensure!(
            summary.unresolved_attempts == 0 && summary.finalized_failures == 0,
            "retained attempts require reconciliation or application review"
        );
        self.controls.journal.update(|image| {
            if Some(image.revision) != self.recovered_generation {
                return Err(JournalError::Conflict);
            }
            image.control.submissions_paused = false;
            image.control.allow_obligations = false;
            Ok(())
        })?;
        Ok(())
    }

    /// Evaluate every signed operation against a live runtime and persist the
    /// result. Call it for each new block or metadata change; deferrals clear
    /// automatically when the live metadata again admits a safe encoding.
    pub fn evaluate_runtime(
        &self,
        runtime: &RuntimeHandle,
        block: u64,
    ) -> Result<ReadinessSummary> {
        self.evaluate_runtime_with(runtime, &LiveFacts::default(), block)
    }

    /// [`Self::evaluate_runtime`] with chain facts read by name.
    pub fn evaluate_runtime_with(
        &self,
        runtime: &RuntimeHandle,
        facts: &LiveFacts,
        block: u64,
    ) -> Result<ReadinessSummary> {
        self.controls
            .record_readiness(block, evaluate_operations_with(runtime, facts))
    }

    /// Refresh live metadata, read [`LiveFacts`] by name and record readiness.
    /// Returns the summary and the runtime facts observed (never compared).
    pub async fn evaluate_live(
        &self,
        client: &Client,
        block: u64,
    ) -> Result<(ReadinessSummary, RuntimeSeen)> {
        let runtime = client.refresh_runtime().await?;
        let facts = read_live_facts(client).await;
        let summary = self.evaluate_runtime_with(&runtime, &facts, block)?;
        Ok((
            summary,
            RuntimeSeen {
                spec_version: runtime.spec_version(),
                transaction_version: runtime.transaction_version(),
            },
        ))
    }

    /// Advance only previously enqueued work through rtao's actual durable dispatch.
    /// The caller provides its signing context after signer-free recovery/resume.
    /// No callback can create uncorrelated or unauthenticated replacement rows.
    ///
    /// The live runtime is re-read and every operation re-evaluated first. A
    /// deferred call does not stop finalized observation, reveal tracking or
    /// the other call; rtao refuses to compose it, the refusal is recorded, and
    /// the next block evaluates it again. An `upstream_storage_refused`
    /// deferral is the exception: rtao reads that item neither through its
    /// compiled descriptor nor by name, so its recovery and both calls stop
    /// until rtao supports the live item. Nothing here compares spec numbers.
    ///
    /// The callback proposes `HoldPrevious` for every mechanism, so only
    /// queued work is ever signed, each under the owner-row policy journaled
    /// with it ([`Self::enqueue_with_owner_rows`]).
    pub async fn dispatch_finalized(
        &mut self,
        cx: &NeuronCx,
        finalized: &BlockHeader,
    ) -> Result<JournalSummary> {
        ensure!(
            self.recovered,
            "recover finalized history before attaching a signer"
        );
        ensure!(!self.controls.inspect()?.paused, "submissions are paused");
        ensure!(
            cx.client.genesis() == self.config.identity.genesis
                && cx.executor.client().genesis() == self.config.identity.genesis
                && cx.netuid == self.config.identity.netuid
                && cx.hotkey.hotkey_address()? == self.config.identity.hotkey,
            "dispatch context identity differs"
        );
        // Readiness is recorded before rtao runs, so a refusal inside rtao
        // (including one that fails its finalized recovery) is visible.
        self.evaluate_live(&cx.client, finalized.number).await?;
        let attempts_before = self.controls.inspect()?.attempts;
        if let Err(error) = self
            .scheduler
            .on_finalized(cx, finalized, |_| WeightsProposal::HoldPrevious)
            .await
        {
            // rtao turns every per-mechanism refusal into a recorded outcome
            // and returns `Ok`; an `Err` comes from its finalized recovery, a
            // typed storage read or RPC. Whether recovery still holds depends
            // only on what was signed: if nothing was, the next block retries
            // (rtao re-runs its recovery first) without a new explicit round.
            let summary = self.controls.inspect()?;
            if !keeps_recovery_after_error(attempts_before, &summary) {
                self.recovered = false;
                self.recovered_generation = None;
            }
            return Err(error.context(
                "rtao weight scheduler returned an error; readiness records any deferred operation",
            ));
        }
        let summary = self.controls.inspect()?;
        if summary.unresolved_attempts != 0 || summary.finalized_failures != 0 {
            self.recovered = false;
            self.recovered_generation = None;
        }
        Ok(summary)
    }
}

/// After rtao's scheduler returned `Err`, in-process recovery still holds only
/// when that call signed nothing: no attempt was added and no attempt is
/// unresolved or finalized failed.
pub fn keeps_recovery_after_error(attempts_before: usize, after: &JournalSummary) -> bool {
    after.attempts == attempts_before
        && after.unresolved_attempts == 0
        && after.finalized_failures == 0
}

fn pause_epoch(
    image: &rtao_client::journal::JournalImage,
) -> std::result::Result<u64, JournalError> {
    let saved = image
        .extensions
        .get(APPLICATION_KEY)
        .ok_or(JournalError::Identity)?;
    let identity: ApplicationIdentity =
        serde_json::from_value(saved.clone()).map_err(|_| JournalError::Identity)?;
    Ok(identity.pause_epoch)
}

fn advance_pause_epoch(
    image: &mut rtao_client::journal::JournalImage,
) -> std::result::Result<(), JournalError> {
    let saved = image
        .extensions
        .get(APPLICATION_KEY)
        .ok_or(JournalError::Identity)?;
    let mut identity: ApplicationIdentity =
        serde_json::from_value(saved.clone()).map_err(|_| JournalError::Identity)?;
    identity.pause_epoch = identity
        .pause_epoch
        .checked_add(1)
        .ok_or_else(|| JournalError::Storage("pause generation exhausted".into()))?;
    image.extensions.insert(
        APPLICATION_KEY.into(),
        serde_json::to_value(identity).map_err(|_| JournalError::Identity)?,
    );
    Ok(())
}

fn canonical_row(mut row: BTreeMap<Uid, f64>) -> Result<BTreeMap<Uid, f64>> {
    ensure!(!row.is_empty(), "weight input is empty");
    let mut positive = false;
    for value in row.values_mut() {
        ensure!(
            value.is_finite() && *value >= 0.0,
            "weight input must be finite and nonnegative"
        );
        positive |= *value > 0.0;
        if *value == 0.0 {
            *value = 0.0;
        }
    }
    if !positive {
        bail!("weight input is all zero");
    }
    Ok(row)
}

/// The row hash `enqueue` records in an operation's correlation, for a
/// semantic guard that must bind a signing attempt to the row it verified.
pub fn weight_row_hash(row: BTreeMap<Uid, f64>) -> Result<String> {
    Ok(row_hash(&canonical_row(row)?))
}

fn row_hash(row: &BTreeMap<Uid, f64>) -> String {
    let mut hash = Sha256::new();
    hash.update(ROW_DOMAIN);
    hash.update((row.len() as u64).to_be_bytes());
    for (uid, value) in row {
        hash.update(uid.0.to_be_bytes());
        hash.update(value.to_bits().to_be_bytes());
    }
    hex::encode(hash.finalize())
}

#[cfg(test)]
mod tests;
