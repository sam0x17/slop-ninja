//! `reward-v1` bundles: the coordinator's signed row and the authorization
//! it was committed under (the copy model, user decision of 2026-11-04).
//!
//! Every validator other than the coordinator commits the coordinator's row
//! and does nothing else. The bundle (schema
//! `slop_ninja.coordinator_row_bundle/v1`) carries three signed records, all
//! under the trusted authorization key: the launch or reward-intent
//! authorization, any renew-by hint refresh of it, and the
//! [`slop_ninja_protocol::CoordinatorRow`] the coordinator signed when it
//! committed that row. The fetcher assembles it from the public pointer
//! (`v1/authorizations/current.json`); without `[fetch]` the operator installs
//! one (testnet shortcut T12). Verification is the protocol crate's
//! `coordinator_row::verify`: both records authenticate and name this chain,
//! the row names the authorization's digest, target, cohort and issue, and
//! its target is the held one (same activation) or one activated later. No
//! score package, receipt, published index or closure is read, and nothing
//! is recomputed: those stay coordinator-private. Each per-tempo commitment
//! checks the authorization again for its epoch and against this
//! validator's journaled revocations ([`crate::revocations`]). No
//! authorization expires: its renew-by hint never refuses a commitment, and
//! the last installed row keeps committing through coordinator silence.
//!
//! The row committed is the record's entries
//! ([`slop_ninja_protocol::VerifiedRow::derive_row`]) with one score-free
//! rule applied against this validator's own finalized snapshot, the rule
//! the coordinator's own derivation applies: the burn entry, and any entry
//! whose `(uid, hotkey, registration block)` the snapshot no longer holds,
//! go to the UID the snapshot's owner hotkey holds (`row_deferred` when
//! units must burn and it holds none). A UID that changed hands is never
//! paid to its new holder, through coordinator silence included; each such
//! entry is logged (`row_uid_moved`). The snapshot also supplies the
//! owner-row policy and the row's registration metadata.
//!
//! Earlier bundle schemas (`slop_ninja.reward_v1_bundle/v1` and `/v3`,
//! which carried the cohort's records and receipts for recomputation) are
//! retired (`bundle_schema_retired`): an installed one is a deferral, never
//! a halt, so an upgraded validator keeps its process, its recovery and its
//! fetcher running and commits again as soon as a row bundle is installed
//! ([`crate::app`]). Nothing here reads a runtime version.

use std::collections::BTreeMap;

use rtao_primitives::{MechId, Uid};
use serde::{Deserialize, Serialize};
use slop_ninja_protocol::{
    AccountId, ActiveTarget, ChainBinding, Digest, NeuronIdentity, Record, RegistrationSnapshot, RegistrationSnapshotV2,
    RewardIntentRevocation, RowAuthorizing, RowSource, TrustedKeys, VerifiedRow,
};
use slop_ninja_subnet::WeightWork;

use crate::package::{Derived, PackageSummary, Refusal, protocol};

/// The coordinator-row bundle's schema.
pub const BUNDLE_SCHEMA_ROW: &str = "slop_ninja.coordinator_row_bundle/v1";
/// Retired: a cohort's records and receipts embedded for recomputation.
pub const BUNDLE_SCHEMA_V2: &str = "slop_ninja.reward_v1_bundle/v1";
/// Retired: the same with receipts by reference.
pub const BUNDLE_SCHEMA_V3: &str = "slop_ninja.reward_v1_bundle/v3";

/// The bundle file: the authorization's kind and signed bytes, any hint
/// refresh, and the coordinator's signed row (each lowercase hex).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RowBundle {
    pub schema: String,
    /// `launch` or `cohort`.
    pub kind: String,
    pub authorization: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub renewal: Option<String>,
    pub row: String,
}

/// Whether bundle bytes are a reward-v1 bundle of any schema (the row
/// bundle, or a retired one, which [`verify`] refuses with its own code).
pub fn is_v2(bytes: &[u8]) -> bool {
    serde_json::from_slice::<serde_json::Value>(bytes)
        .ok()
        .and_then(|v| {
            v.get("schema")
                .and_then(|s| s.as_str())
                .map(|s| s == BUNDLE_SCHEMA_ROW || s == BUNDLE_SCHEMA_V2 || s == BUNDLE_SCHEMA_V3)
        })
        .unwrap_or(false)
}

fn unhex(field: &str, value: &str) -> Result<Vec<u8>, Refusal> {
    hex::decode(value).map_err(|_| Refusal::new("bundle_malformed", format!("{field} is not hex")))
}

/// A verified row bundle with the keys and this validator's journaled
/// revocations, so each per-tempo commitment can check the authorization
/// again.
#[derive(Debug)]
pub struct Authorized {
    pub verified: VerifiedRow,
    pub keys: TrustedKeys,
    /// This validator's journaled revocations, refreshed by the dispatch loop
    /// from [`crate::revocations`] ([`Self::set_revocations`]).
    revocations: std::sync::Mutex<Vec<RewardIntentRevocation>>,
}

impl Authorized {
    /// The authorization for `hotkey` at `epoch`: refused only when the
    /// target is not the held active one, the epoch precedes the
    /// authorization's issue or the target's activation
    /// (`authorization_refused`), or a journaled revocation withdraws it
    /// (`authorization_revoked`). It has no end epoch.
    pub fn check_epoch(&self, hotkey: [u8; 32], epoch: u64) -> Result<String, Refusal> {
        let revocations = self.revocations.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clone();
        self.verified
            .operation(&revocations, &self.verified.active, AccountId(hotkey), epoch)
            .map(|op| op.id())
            .map_err(|e| match e {
                slop_ninja_protocol::Error::Revoked(_) => protocol("authorization_revoked")(e),
                e => protocol("authorization_refused")(e),
            })
    }

    /// Replaces the revocations every later check uses with `loaded`'s:
    /// its records, and each unreadable entry whose target cannot be
    /// recovered applied to this bundle's own target, so it withdraws this
    /// authorization when it was issued at or before the entry's epoch
    /// ([`crate::revocations::Unrecovered`]).
    pub fn set_revocations(&self, loaded: &crate::revocations::Loaded) {
        let v = &self.verified;
        let mut revocations = loaded.records.clone();
        revocations.extend(
            loaded.unrecovered.iter().map(|u| u.against(&v.chain, v.target_digest(), v.authorization_digest)),
        );
        *self.revocations.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = revocations;
    }

    /// The digest of the journaled revocation that withdraws this bundle's
    /// authorization, if any.
    pub fn revoked_by(&self) -> Option<Digest> {
        let revocations = self.revocations.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clone();
        self.verified.revoked_by(&revocations).ok().flatten()
    }

    pub fn summary(&self) -> PackageSummary {
        let v = &self.verified;
        let (cohort, cohort_label, manifest) = match &v.row.source {
            RowSource::Cohort { cohort, cohort_label, cohort_manifest } => {
                (*cohort, cohort_label.clone(), cohort_manifest.to_string())
            }
            RowSource::LaunchBurn => (0, "launch-burn".into(), String::new()),
        };
        PackageSummary {
            cohort,
            cohort_label,
            revision: 0,
            mechanism: v.chain.mechanism,
            manifest,
            package: String::new(),
            target: v.target_digest().to_string(),
            receipt_root: String::new(),
            receipt_count: 0,
            scored_miners: v.row.entries.len(),
        }
    }
}

/// Parses and verifies a coordinator-row bundle for this chain and subnet,
/// for a validator whose own checked history holds `held` active. The row's
/// target must be the held one or one activated later; the verified
/// [`VerifiedRow::active`] is what the caller records next.
pub fn verify(
    bytes: &[u8],
    keys: &TrustedKeys,
    genesis: &[u8; 32],
    netuid: u16,
    held: &ActiveTarget,
) -> Result<Authorized, Refusal> {
    if bytes.len() as u64 > crate::package::MAX_BUNDLE_BYTES {
        return Err(Refusal::new("bundle_too_large", "bundle exceeds the file bound"));
    }
    let schema = serde_json::from_slice::<serde_json::Value>(bytes)
        .ok()
        .and_then(|v| v.get("schema").and_then(|s| s.as_str()).map(str::to_owned));
    if matches!(schema.as_deref(), Some(BUNDLE_SCHEMA_V2 | BUNDLE_SCHEMA_V3)) {
        return Err(Refusal::new(
            "bundle_schema_retired",
            "validators commit the coordinator's signed row (schema slop_ninja.coordinator_row_bundle/v1); \
             bundles of cohort records and receipts are no longer accepted",
        ));
    }
    let b: RowBundle =
        serde_json::from_slice(bytes).map_err(|error| Refusal::new("bundle_malformed", error.to_string()))?;
    if b.schema != BUNDLE_SCHEMA_ROW {
        return Err(Refusal::new("bundle_schema", "unsupported bundle schema"));
    }
    let chain = ChainBinding { genesis: Digest(*genesis), netuid, mechanism: 0 };
    let auth = unhex("authorization", &b.authorization)?;
    let renewal = b.renewal.as_deref().map(|r| unhex("renewal", r)).transpose()?;
    let row = unhex("row", &b.row)?;
    let authorizing = match b.kind.as_str() {
        "launch" => RowAuthorizing::Launch(&auth),
        "cohort" => RowAuthorizing::Cohort(&auth),
        _ => return Err(Refusal::new("bundle_schema", "authorization kind")),
    };
    let verified = slop_ninja_protocol::coordinator_row::verify(&chain, keys, held, authorizing, renewal.as_deref(), &row)
        .map_err(protocol("bundle_unverified"))?;
    MechId::new(verified.chain.mechanism).map_err(|error| Refusal::new("package_mechanism", error.to_string()))?;
    Ok(Authorized { verified, keys: keys.clone(), revocations: Default::default() })
}

/// The row bundle of the signed records (the fetcher's and the operator
/// tool's form).
pub fn row_bundle(kind: &str, authorization: &[u8], renewal: Option<&[u8]>, row: &[u8]) -> RowBundle {
    RowBundle {
        schema: BUNDLE_SCHEMA_ROW.into(),
        kind: kind.into(),
        authorization: hex::encode(authorization),
        renewal: renewal.map(hex::encode),
        row: hex::encode(row),
    }
}

/// The reward-v1 snapshot: this validator's finalized registrations and the
/// finalized subnet owner hotkey.
pub fn snapshot(v1: &RegistrationSnapshot, owner_hotkey: [u8; 32]) -> Result<RegistrationSnapshotV2, Refusal> {
    let s = RegistrationSnapshotV2 {
        chain: v1.chain.clone(),
        block: v1.block,
        block_hash: v1.block_hash,
        owner_hotkey: AccountId(owner_hotkey),
        neurons: v1.neurons.iter().map(|n| NeuronIdentity { ..*n }).collect(),
    };
    s.validate().map_err(protocol("registration_snapshot_invalid"))?;
    Ok(s)
}

/// The row and work this validator commits at `epoch`, under the bundle's
/// checked authorization: the coordinator's signed entries, with the units
/// of registrations the snapshot no longer holds burned (module docs).
pub fn derive(
    a: std::sync::Arc<Authorized>,
    snapshot: &RegistrationSnapshotV2,
    hotkey: [u8; 32],
    epoch: u64,
) -> Result<Derived, Refusal> {
    let row = a.verified.derive_row(snapshot).map_err(|e| match e {
        slop_ninja_protocol::Error::Deferred(why) => {
            Refusal::new("row_deferred", format!("{why}: units must burn and the owner hotkey holds no UID"))
        }
        e => protocol("row_refused")(e),
    })?;
    let moved = a.verified.moved_uids(snapshot);
    if !moved.is_empty() {
        tracing::warn!(event = "row_uid_moved", uids = ?moved, epoch, note = "burned to the owner UID, never paid to the new registration");
    }
    a.check_epoch(hotkey, epoch)?;
    let label = a.summary().cohort_label;
    let target = a.verified.target_digest();
    let work = WeightWork {
        manifest_hash: target.0,
        cohort: label,
        revision: 0,
        mechanism: MechId(a.verified.chain.mechanism),
        tempo: Some(epoch),
        v2_target: Some(target.0),
    };
    // The canonical row sums to 65,535; it is committed max-upscaled, the
    // form rtao's preparation and the chain's reveal both keep.
    let chain_entries = row.chain_values();
    let input: BTreeMap<Uid, f64> = chain_entries.iter().map(|&(uid, w)| (Uid(uid), f64::from(w))).collect();
    Ok(Derived {
        work,
        row_digest: row.digest().map(|d| d.to_string()).unwrap_or_default(),
        row_entries: row.entries.iter().map(|e| (e.uid, e.weight)).collect(),
        chain_entries,
        input,
        reward_v1: Some(a),
        snapshot_block: Some(snapshot.block),
    })
}

/// This validator's own durable record of the target it holds active, kept
/// beside the journal. It is written only from a verified bundle's
/// [`VerifiedRow::active`] (the launch burn row, or the target the
/// coordinator's signed row names with its activation) and only ever moves
/// forward, so a superseded bundle or the launch authorization after the
/// first activation is refused.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HeldTargetFile {
    pub schema: String,
    pub genesis: String,
    pub netuid: u16,
    pub mechanism: u8,
    pub target: String,
    /// `None` for the launch burn row.
    pub activated_epoch: Option<u64>,
    /// [`authority_digest`] of the trusted keys the record was written
    /// under. A record of another authority (rotated trust roots, a
    /// restarted chain record) is set aside and the validator starts again
    /// from the launch burn row; a record without it is bound to the current
    /// keys when next written.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub authority: Option<String>,
    /// P1-V: the issue epoch of the latest restart's launch authorization
    /// this validator accepted, while it holds the launch burn row: a row
    /// that activated before it, or a launch row issued before it, is
    /// refused (`row_before_restart`, `restart_stale`), so no pre-restart
    /// row verifies again, also after a validator restart. `None` in a v1
    /// file, and before any restart.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub restart_floor: Option<u64>,
}

/// The held-target file's schema: v2 adds `restart_floor`. A v1 file is read
/// as floor `None` and rewritten as v2 on the next record; a build that only
/// reads v1 refuses a v2 file, so a rollback over P1-V rolls forward only.
pub const HELD_TARGET_SCHEMA: &str = "slop_ninja.validator_held_target/v2";
/// The schema before P1-V, still read.
pub const HELD_TARGET_SCHEMA_V1: &str = "slop_ninja.validator_held_target/v1";

/// The authority a held-target record belongs to: SHA-256 over a domain tag
/// and the package, receipt and authorization public keys.
pub fn authority_digest(keys: &TrustedKeys) -> String {
    use sha2::{Digest as _, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(b"slop_ninja.validator_held_authority/v1");
    for role in [
        slop_ninja_protocol::KeyRole::Package,
        slop_ninja_protocol::KeyRole::Receipt,
        slop_ninja_protocol::KeyRole::Authorization,
    ] {
        hasher.update(keys.key(role));
    }
    hex::encode(hasher.finalize())
}

impl HeldTargetFile {
    pub fn of(active: &ActiveTarget) -> Self {
        Self {
            schema: HELD_TARGET_SCHEMA.into(),
            genesis: hex::encode(active.chain().genesis.0),
            netuid: active.chain().netuid,
            mechanism: active.chain().mechanism,
            target: hex::encode(active.digest().0),
            activated_epoch: active.activated_epoch(),
            authority: None,
            restart_floor: None,
        }
    }

    /// The record with restart floor `floor` (P1-V).
    pub fn with_floor(mut self, floor: Option<u64>) -> Self {
        self.restart_floor = floor;
        self
    }

    /// The record bound to `keys`.
    pub fn bound_to(mut self, keys: &TrustedKeys) -> Self {
        self.authority = Some(authority_digest(keys));
        self
    }

    pub fn restore(&self, chain: &ChainBinding) -> Result<ActiveTarget, Refusal> {
        let bad = |what: &str| Refusal::new("held_target_invalid", what.to_owned());
        if self.schema != HELD_TARGET_SCHEMA && self.schema != HELD_TARGET_SCHEMA_V1 {
            return Err(bad("schema"));
        }
        if self.genesis != hex::encode(chain.genesis.0) || self.netuid != chain.netuid || self.mechanism != chain.mechanism {
            return Err(bad("chain"));
        }
        let digest: [u8; 32] = hex::decode(&self.target)
            .ok()
            .and_then(|b| b.try_into().ok())
            .ok_or_else(|| bad("target"))?;
        ActiveTarget::restore(chain, Digest(digest), self.activated_epoch).map_err(protocol("held_target_invalid"))
    }
}

/// Whether `next` moves the held target forward: the same target, or one
/// activated later. The launch burn row never follows an activated target.
pub fn advances(held: &ActiveTarget, next: &ActiveTarget) -> bool {
    match (held.activated_epoch(), next.activated_epoch()) {
        (_, None) => held.is_launch_burn() && next.is_launch_burn(),
        (None, Some(_)) => true,
        (Some(a), Some(b)) => b > a || (b == a && held.digest() == next.digest()),
    }
}
