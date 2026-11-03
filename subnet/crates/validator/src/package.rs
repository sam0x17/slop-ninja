//! Coordinator-signed score packages, verified before any row is enqueued.
//!
//! Testnet shortcut T12: the package arrives as one local bundle file holding
//! the signed cohort manifest, score package, calculation target, receipt index
//! and every receipt object. M6 replaces the file with the coordinator's
//! published package and R2 receipts; the verification below is unchanged.
//!
//! Verification authenticates every signed record before decoding it
//! (`Signed::open`), binds the package to its manifest, the target to the
//! package and the receipt index to the package root and count, and checks
//! each stored receipt object against its index entry. Nothing here reads the
//! runtime version; a package stays valid across runtime upgrades.

use std::collections::BTreeMap;

use rtao_primitives::{MechId, Uid};
use serde::{Deserialize, Serialize};
use slop_ninja_protocol::{
    CalculationTarget, ChainBinding, CohortManifest, Digest, NeuronIdentity, ReceiptIndex, Record,
    RegistrationSnapshot, ScorePackage, Signed, TrustedKeys,
};
use slop_ninja_subnet::WeightWork;

/// Bundle schema identifier.
pub const BUNDLE_SCHEMA: &str = "slop_ninja.fixture_package/v1";
/// Largest bundle file accepted.
pub const MAX_BUNDLE_BYTES: u64 = 64 << 20;

/// One package as stored on disk. Every field is the hex of exact bytes.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Bundle {
    pub schema: String,
    /// Always `T12`: this file format is a labelled testnet shortcut.
    pub shortcut: String,
    /// `Signed<CohortManifest>` envelope (package key).
    pub manifest: String,
    /// `Signed<ScorePackage>` envelope (package key).
    pub package: String,
    /// `Signed<CalculationTarget>` envelope (package key).
    pub target: String,
    /// `Signed<ReceiptIndex>` envelope (receipt key).
    pub receipt_index: String,
    /// Stored `AttemptReceipt` objects, in receipt-index order.
    pub receipts: Vec<String>,
}

/// A verification or derivation refusal with a stable code.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Refusal {
    pub code: &'static str,
    pub detail: String,
}

impl Refusal {
    pub(crate) fn new(code: &'static str, detail: impl Into<String>) -> Self {
        Self {
            code,
            detail: detail.into(),
        }
    }
}

impl std::fmt::Display for Refusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.code, self.detail)
    }
}

impl std::error::Error for Refusal {}

pub(crate) fn protocol(code: &'static str) -> impl Fn(slop_ninja_protocol::Error) -> Refusal {
    move |error| Refusal::new(code, format!("{} ({})", error, error.code()))
}

fn unhex(field: &str, value: &str) -> Result<Vec<u8>, Refusal> {
    hex::decode(value).map_err(|_| Refusal::new("bundle_malformed", format!("{field} is not hex")))
}

impl Bundle {
    pub fn parse(bytes: &[u8]) -> Result<Self, Refusal> {
        if bytes.len() as u64 > MAX_BUNDLE_BYTES {
            return Err(Refusal::new("bundle_too_large", "bundle exceeds 64 MiB"));
        }
        let bundle: Self = serde_json::from_slice(bytes)
            .map_err(|error| Refusal::new("bundle_malformed", error.to_string()))?;
        if bundle.schema != BUNDLE_SCHEMA || bundle.shortcut != "T12" {
            return Err(Refusal::new(
                "bundle_schema",
                "unsupported bundle schema or shortcut label",
            ));
        }
        Ok(bundle)
    }

    pub fn to_json(&self) -> String {
        serde_json::to_string_pretty(self).expect("bundle serializes")
    }
}

/// Authenticated, mutually bound records of one package.
#[derive(Clone, Debug)]
pub struct VerifiedPackage {
    pub manifest: CohortManifest,
    pub package: ScorePackage,
    pub target: CalculationTarget,
    pub receipt_count: u64,
}

/// Content-free identities for logs and dry-run output.
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct PackageSummary {
    pub cohort: u64,
    pub cohort_label: String,
    pub revision: u64,
    pub mechanism: u8,
    pub manifest: String,
    pub package: String,
    pub target: String,
    pub receipt_root: String,
    pub receipt_count: u64,
    pub scored_miners: usize,
}

impl VerifiedPackage {
    pub fn summary(&self) -> PackageSummary {
        PackageSummary {
            cohort: self.target.cohort,
            cohort_label: self.target.cohort_label.clone(),
            revision: self.target.revision,
            mechanism: self.target.chain.mechanism,
            manifest: self.target.cohort_manifest.to_string(),
            package: self.target.score_package.to_string(),
            target: self
                .target
                .digest()
                .map(|d| d.to_string())
                .unwrap_or_default(),
            receipt_root: self.target.receipt_root.to_string(),
            receipt_count: self.receipt_count,
            scored_miners: self.package.scores.len(),
        }
    }

    /// The chain binding every record shares.
    pub fn chain(&self) -> &ChainBinding {
        &self.target.chain
    }
}

/// Verify a bundle for `genesis` and `netuid`. The mechanism comes from the
/// package. Every refusal happens before any row exists.
pub fn verify(
    bundle: &Bundle,
    keys: &TrustedKeys,
    genesis: &[u8; 32],
    netuid: u16,
) -> Result<VerifiedPackage, Refusal> {
    let manifest = Signed::<CohortManifest>::open(&unhex("manifest", &bundle.manifest)?, keys)
        .map_err(protocol("manifest_unverified"))?;
    let package = Signed::<ScorePackage>::open(&unhex("package", &bundle.package)?, keys)
        .map_err(protocol("package_unverified"))?;
    let target = Signed::<CalculationTarget>::open(&unhex("target", &bundle.target)?, keys)
        .map_err(protocol("target_unverified"))?;
    let index = Signed::<ReceiptIndex>::open(&unhex("receipt_index", &bundle.receipt_index)?, keys)
        .map_err(protocol("receipt_index_unverified"))?;
    if package.chain.genesis.0 != *genesis || package.chain.netuid != netuid {
        return Err(Refusal::new(
            "package_chain_mismatch",
            "the package names another chain or subnet than this validator",
        ));
    }
    MechId::new(package.chain.mechanism)
        .map_err(|error| Refusal::new("package_mechanism", error.to_string()))?;
    package
        .check_manifest(&manifest)
        .map_err(protocol("package_manifest_mismatch"))?;
    target
        .check_package(&package)
        .map_err(protocol("target_package_mismatch"))?;
    package
        .check_receipts(&index)
        .map_err(protocol("receipt_index_mismatch"))?;
    if bundle.receipts.len() as u64 != index.count() {
        return Err(Refusal::new(
            "receipts_incomplete",
            format!(
                "{} receipt objects for {} index entries",
                bundle.receipts.len(),
                index.count()
            ),
        ));
    }
    let baseline = manifest.baseline_artifact;
    for (position, object) in bundle.receipts.iter().enumerate() {
        let receipt = index
            .verify_object(position, &unhex("receipts", object)?)
            .map_err(protocol("receipt_object_mismatch"))?;
        if receipt.artifact != baseline
            && manifest.artifacts.binary_search(&receipt.artifact).is_err()
        {
            return Err(Refusal::new(
                "receipt_artifact_unknown",
                "a receipt names an artifact outside the manifest",
            ));
        }
    }
    Ok(VerifiedPackage {
        receipt_count: index.count(),
        manifest,
        package,
        target,
    })
}

/// A validator's own finalized registrations, as read from chain storage.
pub fn snapshot(
    chain: &ChainBinding,
    block: u64,
    block_hash: [u8; 32],
    mut neurons: Vec<NeuronIdentity>,
) -> Result<RegistrationSnapshot, Refusal> {
    neurons.sort_by_key(|neuron| neuron.uid);
    let snapshot = RegistrationSnapshot {
        chain: chain.clone(),
        block,
        block_hash: Digest(block_hash),
        neurons,
    };
    snapshot
        .validate()
        .map_err(protocol("registration_snapshot_invalid"))?;
    Ok(snapshot)
}

/// The row this validator commits for a verified package.
#[derive(Clone, Debug)]
pub struct Derived {
    pub work: WeightWork,
    /// The canonical row's digest (a `weight-row` v1 or v2 record).
    pub row_digest: String,
    /// The canonical row by UID.
    pub row_entries: Vec<(u16, u16)>,
    /// The row as committed and as the chain stores it after a reveal: the
    /// canonical row max-upscaled so its largest entry is 65,535
    /// (`WeightRowV2::chain_values`). A `testnet-reward-v0` row already has
    /// that form, so it equals `row_entries`.
    pub chain_entries: Vec<(u16, u16)>,
    /// `chain_entries` as the adapter's input. Every weight is an exact
    /// integer whose maximum is 65535, so rtao's max-upscale keeps it.
    pub input: BTreeMap<Uid, f64>,
    /// `reward-v1`: the verified bundle, whose authorization every per-tempo
    /// commitment re-checks.
    pub reward_v1: Option<std::sync::Arc<crate::package_v2::Authorized>>,
    /// `reward-v1`: the finalized snapshot block the row was derived at.
    pub snapshot_block: Option<u64>,
}

impl Derived {
    pub fn row_digest(&self) -> String {
        self.row_digest.clone()
    }

    pub fn entries(&self) -> Vec<(u16, u16)> {
        self.row_entries.clone()
    }

    /// The committed form, which a revealed `Weights` row must equal.
    pub fn chain_entries(&self) -> Vec<(u16, u16)> {
        self.chain_entries.clone()
    }

    /// The work of the tempo `epoch` re-commit, after this bundle's
    /// authorization is checked again for that epoch (reward-v1).
    pub fn work_for_tempo(&self, hotkey: [u8; 32], epoch: u64) -> Result<WeightWork, Refusal> {
        if let Some(a) = &self.reward_v1 {
            a.check_epoch(hotkey, epoch)?;
        }
        Ok(self.work.for_tempo(epoch))
    }
}

/// Derive the native row with the shared protocol crate.
pub fn derive(
    verified: &VerifiedPackage,
    snapshot: &RegistrationSnapshot,
) -> Result<Derived, Refusal> {
    let row =
        slop_ninja_protocol::reward::derive_row(&verified.target, &verified.package, snapshot)
            .map_err(protocol("row_refused"))?;
    let input = row
        .entries
        .iter()
        .map(|entry| (Uid(entry.uid), f64::from(entry.weight)))
        .collect();
    let row_entries: Vec<(u16, u16)> = row.entries.iter().map(|e| (e.uid, e.weight)).collect();
    Ok(Derived {
        row_digest: row.digest().map(|d| d.to_string()).unwrap_or_default(),
        chain_entries: row_entries.clone(),
        row_entries,
        reward_v1: None,
        snapshot_block: None,
        work: WeightWork {
            manifest_hash: verified.target.cohort_manifest.0,
            cohort: verified.target.cohort_label.clone(),
            revision: verified.target.revision,
            mechanism: MechId(verified.target.chain.mechanism),
            tempo: None,
            v2_target: None,
        },
        input,
    })
}
