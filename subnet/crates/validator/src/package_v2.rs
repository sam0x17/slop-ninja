//! `reward-v1` bundles (T12 shortcut): the active target's records and the
//! authorization that lets this validator commit it.
//!
//! The coordinator's `reward_v1_verify` writes the bundle from the receipts
//! bucket. Verification is the protocol crate's `target_bundle`: every signed
//! record is authenticated before it is decoded; the package is bound to its
//! manifest, the target to the package and the receipt index to the package's
//! root and count; every retained receipt chunk is checked against the index
//! (each entry exactly once); the published index must match the manifest's
//! binding; the outcome is recomputed (`check_outcome`), so a package whose
//! credits or units differ from the rule is refused before any row exists; the
//! closure is checked against the manifest's frozen minimums and activates the
//! target; and the reward-intent (or launch) authorization is checked against
//! the active target and its carrying cohort. Each per-tempo commitment checks
//! the authorization again for its epoch and against this validator's
//! journaled revocations ([`crate::revocations`]). No authorization expires:
//! its renew-by hint never refuses a commitment.
//!
//! Bundle schema v3 (`slop_ninja.reward_v1_bundle/v3`) carries the same
//! records, except that the receipt indexes and receipt chunk objects of the
//! target's cohort and of any carrier are not embedded: each is named by an
//! absolute file path, its SHA-256 and its length ([`ObjectRef`]), and is
//! read from that file, checked against both and verified one chunk at a
//! time. Embedding them hex-encoded limits a cohort and its carrier to about
//! 150,000 receipts within the 64 MiB file bound; by reference, the receipt
//! index's own sequence bound is the limit. The fetcher keeps them in its
//! verified cache; the operator's tool writes them to a directory it names.
//!
//! The row is `reward_v1::derive_row` against this validator's own finalized
//! snapshot, including the finalized subnet owner hotkey, which receives any
//! burned units. Nothing here reads a runtime version.

use std::collections::BTreeMap;

use rtao_primitives::{MechId, Uid};
use serde::{Deserialize, Serialize};
use slop_ninja_protocol::{
    AccountId, ActiveTarget, ChainBinding, Digest, NeuronIdentity, Record, RegistrationSnapshot,
    RegistrationSnapshotV2, RewardIntentRevocation, TrustedKeys, VerifiedTarget,
};
use slop_ninja_subnet::WeightWork;

use crate::package::{Derived, PackageSummary, Refusal, protocol};

pub const BUNDLE_SCHEMA_V2: &str = "slop_ninja.reward_v1_bundle/v1";
/// Bundle schema v3: receipt indexes and chunks by reference ([`ObjectRef`]).
pub const BUNDLE_SCHEMA_V3: &str = "slop_ninja.reward_v1_bundle/v3";

/// An object a v3 bundle names instead of embedding: an absolute path, the
/// object's SHA-256 (lowercase hex) and its exact length.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ObjectRef {
    pub path: String,
    pub sha256: String,
    pub bytes: u64,
}

/// A cohort's signed receipt index and its receipt chunk objects, by
/// reference.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReceiptsRef {
    pub index: ObjectRef,
    pub chunks: Vec<ObjectRef>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceHex {
    pub manifest: String,
    pub package: String,
    pub target: String,
    pub closure: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub receipt_index: Option<String>,
    pub published_contents: String,
    pub fingerprint_index: String,
}

/// A later no-target cohort that carries the target: its signed manifest,
/// package and receipt index, and its published index as of its manifest.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CarrierHex {
    pub manifest: String,
    pub package: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub receipt_index: Option<String>,
    pub published_contents: String,
    pub fingerprint_index: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AuthorizationHex {
    pub kind: String,
    pub authorization: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub renewal: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub carrier: Option<CarrierHex>,
}

/// The bundle file, schema v2 (`slop_ninja.reward_v1_bundle/v1`) or v3. Field meanings follow the coordinator's
/// `target::BundleV2`; a v3 bundle leaves the receipt indexes and chunk
/// lists empty and names them in `receipts` and `carrier_receipts`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BundleV2 {
    pub schema: String,
    pub shortcut: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<SourceHex>,
    pub authorization: AuthorizationHex,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub receipt_chunks: Vec<String>,
    /// The carrying cohort's receipt chunk objects (hex).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub carrier_receipt_chunks: Vec<String>,
    /// v3: the target cohort's receipt index and chunks, by reference.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub receipts: Option<ReceiptsRef>,
    /// v3: the carrying cohort's receipt index and chunks, by reference.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub carrier_receipts: Option<ReceiptsRef>,
}

/// Whether bundle bytes are the reward-v1 form (bundle schema v2 or v3).
pub fn is_v2(bytes: &[u8]) -> bool {
    serde_json::from_slice::<serde_json::Value>(bytes)
        .ok()
        .and_then(|v| {
            v.get("schema").and_then(|s| s.as_str()).map(|s| s == BUNDLE_SCHEMA_V2 || s == BUNDLE_SCHEMA_V3)
        })
        .unwrap_or(false)
}

/// Opens a referenced object: an absolute path to a regular file of exactly
/// the named length (checked before reading), whose SHA-256 is the named
/// one. A missing file is `receipts_incomplete`; any other mismatch
/// `receipt_object_mismatch`.
pub fn read_object(r: &ObjectRef, what: &str) -> Result<Vec<u8>, Refusal> {
    use sha2::{Digest as _, Sha256};
    let path = std::path::Path::new(&r.path);
    if !path.is_absolute() {
        return Err(Refusal::new("bundle_malformed", format!("{what}: an object path must be absolute")));
    }
    let meta = match std::fs::metadata(path) {
        Ok(m) => m,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Err(Refusal::new("receipts_incomplete", format!("{what}: the referenced object is missing")));
        }
        Err(e) => return Err(Refusal::new("receipts_incomplete", format!("{what}: {e}"))),
    };
    if !meta.is_file() || meta.len() != r.bytes {
        return Err(Refusal::new("receipt_object_mismatch", format!("{what}: the referenced object's length differs")));
    }
    let bytes = std::fs::read(path).map_err(|e| Refusal::new("receipts_incomplete", format!("{what}: {e}")))?;
    if bytes.len() as u64 != r.bytes || hex::encode(Sha256::digest(&bytes)) != r.sha256.to_ascii_lowercase() {
        return Err(Refusal::new("receipt_object_mismatch", format!("{what}: the referenced object's digest differs")));
    }
    Ok(bytes)
}

/// Verifies referenced chunk objects one at a time.
fn verify_chunk_refs(
    mut v: slop_ninja_protocol::target_bundle::ReceiptChunkVerifier<'_>,
    chunks: &[ObjectRef],
    what: &str,
) -> Result<u64, Refusal> {
    for c in chunks {
        let bytes = read_object(c, what)?;
        v.push(&bytes).map_err(protocol("receipt_object_mismatch"))?;
    }
    v.finish().map_err(protocol("receipt_object_mismatch"))
}

fn unhex(field: &str, value: &str) -> Result<Vec<u8>, Refusal> {
    hex::decode(value).map_err(|_| Refusal::new("bundle_malformed", format!("{field} is not hex")))
}

/// A verified bundle with the keys and this validator's journaled
/// revocations, so each per-tempo commitment can check the authorization
/// again.
#[derive(Debug)]
pub struct Authorized {
    pub verified: VerifiedTarget,
    pub keys: TrustedKeys,
    /// Receipts verified against the index (0 when the bundle retains none),
    /// the carrying cohort's included.
    pub receipts_verified: u64,
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
            .operation(&self.keys, &revocations, AccountId(hotkey), epoch)
            .map(|op| op.id())
            .map_err(|e| match e {
                slop_ninja_protocol::Error::Revoked(_) => protocol("authorization_revoked")(e),
                e => protocol("authorization_refused")(e),
            })
    }

    /// Replaces the revocations every later check uses.
    pub fn set_revocations(&self, revocations: Vec<RewardIntentRevocation>) {
        *self.revocations.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = revocations;
    }

    /// The digest of the journaled revocation that withdraws this bundle's
    /// authorization, if any. An authorization that no longer verifies is
    /// not reported here: [`Self::check_epoch`] refuses it
    /// (`authorization_refused`) with its own cause.
    pub fn revoked_by(&self) -> Option<Digest> {
        let revocations = self.revocations.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clone();
        self.verified.revoked_by(&self.keys, &revocations).ok().flatten()
    }

    pub fn summary(&self) -> PackageSummary {
        let v = &self.verified;
        match &v.source {
            Some(s) => PackageSummary {
                cohort: s.package.cohort,
                cohort_label: s.package.cohort_label.clone(),
                revision: s.package.revision,
                mechanism: v.chain.mechanism,
                manifest: s.manifest.digest().map(|d| d.to_string()).unwrap_or_default(),
                package: s.package.digest().map(|d| d.to_string()).unwrap_or_default(),
                target: v.target_digest.to_string(),
                receipt_root: s.package.receipt_root.to_string(),
                receipt_count: s.package.receipt_count,
                scored_miners: s.package.entrants.iter().filter(|e| e.score.is_some()).count(),
            },
            None => PackageSummary {
                cohort: 0,
                cohort_label: "launch-burn".into(),
                revision: 0,
                mechanism: v.chain.mechanism,
                manifest: String::new(),
                package: String::new(),
                target: v.target_digest.to_string(),
                receipt_root: String::new(),
                receipt_count: 0,
                scored_miners: 0,
            },
        }
    }
}

/// Parses and verifies a reward-v1 bundle for this chain and subnet, for a
/// validator whose own checked history holds `held` active. The bundle's
/// target must be the held one or its successor; the verified
/// [`VerifiedTarget::active`] is what the caller records next.
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
    let b: BundleV2 =
        serde_json::from_slice(bytes).map_err(|error| Refusal::new("bundle_malformed", error.to_string()))?;
    let v3 = b.schema == BUNDLE_SCHEMA_V3;
    if !(v3 || b.schema == BUNDLE_SCHEMA_V2) || b.shortcut != "T12" {
        return Err(Refusal::new("bundle_schema", "unsupported bundle schema or shortcut label"));
    }
    // Each schema carries receipts in its own form only.
    let embedded = b.source.as_ref().is_some_and(|s| s.receipt_index.is_some())
        || b.authorization.carrier.as_ref().is_some_and(|c| c.receipt_index.is_some())
        || !b.receipt_chunks.is_empty()
        || !b.carrier_receipt_chunks.is_empty();
    let referenced = b.receipts.is_some() || b.carrier_receipts.is_some();
    if (v3 && embedded) || (!v3 && referenced) {
        return Err(Refusal::new("bundle_malformed", "receipts in another bundle schema's form"));
    }
    if (b.source.is_none() && b.receipts.is_some()) || (b.authorization.carrier.is_none() && b.carrier_receipts.is_some()) {
        return Err(Refusal::new("bundle_malformed", "receipts without the records they belong to"));
    }
    // v3: the referenced receipt indexes, read and checked against their
    // references before anything is verified.
    let index_ref = |r: &Option<ReceiptsRef>, what: &str| -> Result<Option<Vec<u8>>, Refusal> {
        r.as_ref().map(|r| read_object(&r.index, what)).transpose()
    };
    let (source_index, carrier_index) = if v3 {
        (
            b.source.as_ref().map(|_| index_ref(&b.receipts, "receipts.index")).transpose()?.flatten(),
            b.authorization.carrier.as_ref().map(|_| index_ref(&b.carrier_receipts, "carrier_receipts.index")).transpose()?.flatten(),
        )
    } else {
        (None, None)
    };
    let chain = ChainBinding { genesis: Digest(*genesis), netuid, mechanism: 0 };
    let src = match &b.source {
        Some(s) => Some((
            unhex("manifest", &s.manifest)?,
            unhex("package", &s.package)?,
            unhex("target", &s.target)?,
            unhex("closure", &s.closure)?,
            match source_index {
                Some(bytes) => Some(bytes),
                None => s.receipt_index.as_deref().map(|r| unhex("receipt_index", r)).transpose()?,
            },
            unhex("published_contents", &s.published_contents)?,
            unhex("fingerprint_index", &s.fingerprint_index)?,
        )),
        None => None,
    };
    if src.as_ref().is_some_and(|s| s.4.is_none()) {
        // A validator other than the producer verifies the full retained index.
        return Err(Refusal::new("receipts_incomplete", "the bundle carries no receipt index"));
    }
    let source = src.as_ref().map(|(m, p, t, c, r, pc, fi)| slop_ninja_protocol::Source {
        manifest: m,
        package: p,
        target: t,
        closure: c,
        receipt_index: r.as_deref(),
        published_contents: pc,
        fingerprint_index: fi,
    });
    let auth = unhex("authorization", &b.authorization.authorization)?;
    let renewal = b.authorization.renewal.as_deref().map(|r| unhex("renewal", r)).transpose()?;
    let carrier = match &b.authorization.carrier {
        Some(c) => {
            let receipt_index = match (carrier_index, &c.receipt_index) {
                (Some(bytes), _) => bytes,
                (None, Some(hex)) => unhex("carrier receipt_index", hex)?,
                (None, None) => {
                    return Err(Refusal::new("receipts_incomplete", "the carrier carries no receipt index"));
                }
            };
            Some((
                unhex("carrier manifest", &c.manifest)?,
                unhex("carrier package", &c.package)?,
                receipt_index,
                unhex("carrier published_contents", &c.published_contents)?,
                unhex("carrier fingerprint_index", &c.fingerprint_index)?,
            ))
        }
        None => None,
    };
    let authorizing = match b.authorization.kind.as_str() {
        "launch" => slop_ninja_protocol::Authorizing::Launch { authorization: &auth, renewal: renewal.as_deref() },
        "cohort" => slop_ninja_protocol::Authorizing::Cohort {
            authorization: &auth,
            renewal: renewal.as_deref(),
            carrier: carrier.as_ref().map(|(m, p, r, pc, fi)| slop_ninja_protocol::Carrier {
                manifest: m,
                package: p,
                receipt_index: Some(r),
                published_contents: pc,
                fingerprint_index: fi,
            }),
        },
        _ => return Err(Refusal::new("bundle_schema", "authorization kind")),
    };
    let verified = slop_ninja_protocol::target_bundle::verify(&chain, keys, held, source, authorizing)
        .map_err(protocol("bundle_unverified"))?;
    let mut receipts_verified = 0;
    if v3 {
        if verified.source.is_some() {
            let r = b.receipts.as_ref().ok_or_else(|| Refusal::new("receipts_incomplete", "the bundle names no receipts"))?;
            let v = verified.receipt_verifier().map_err(protocol("receipts_incomplete"))?;
            receipts_verified = verify_chunk_refs(v, &r.chunks, "receipts.chunks")?;
        }
        if verified.carrier().is_some() {
            let r = b
                .carrier_receipts
                .as_ref()
                .ok_or_else(|| Refusal::new("receipts_incomplete", "the bundle names no carrier receipts"))?;
            let v = verified.carrier_receipt_verifier().map_err(protocol("receipts_incomplete"))?;
            receipts_verified += verify_chunk_refs(v, &r.chunks, "carrier_receipts.chunks")?;
        }
        MechId::new(verified.chain.mechanism).map_err(|error| Refusal::new("package_mechanism", error.to_string()))?;
        return Ok(Authorized { verified, keys: keys.clone(), receipts_verified, revocations: Default::default() });
    }
    if verified.source.is_some() {
        if b.receipt_chunks.is_empty() {
            return Err(Refusal::new("receipts_incomplete", "the bundle retains no receipt objects"));
        }
        let chunks = b.receipt_chunks.iter().map(|c| unhex("receipt_chunks", c)).collect::<Result<Vec<_>, _>>()?;
        let refs: Vec<&[u8]> = chunks.iter().map(Vec::as_slice).collect();
        receipts_verified = verified.verify_receipts(&refs).map_err(protocol("receipt_object_mismatch"))?;
    }
    if verified.carrier().is_some() {
        if b.carrier_receipt_chunks.is_empty() {
            return Err(Refusal::new("receipts_incomplete", "the bundle retains no carrier receipt objects"));
        }
        let chunks =
            b.carrier_receipt_chunks.iter().map(|c| unhex("carrier_receipt_chunks", c)).collect::<Result<Vec<_>, _>>()?;
        let refs: Vec<&[u8]> = chunks.iter().map(Vec::as_slice).collect();
        receipts_verified +=
            verified.verify_carrier_receipts(&refs).map_err(protocol("receipt_object_mismatch"))?;
    }
    MechId::new(verified.chain.mechanism).map_err(|error| Refusal::new("package_mechanism", error.to_string()))?;
    Ok(Authorized { verified, keys: keys.clone(), receipts_verified, revocations: Default::default() })
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
/// checked authorization.
pub fn derive(
    a: std::sync::Arc<Authorized>,
    snapshot: &RegistrationSnapshotV2,
    hotkey: [u8; 32],
    epoch: u64,
) -> Result<Derived, Refusal> {
    let row = match a.verified.derive_row(snapshot) {
        Ok(row) => row,
        Err(slop_ninja_protocol::Error::Deferred(why)) => {
            return Err(Refusal::new("row_deferred", format!("{why}: units must burn and the owner hotkey holds no UID")));
        }
        Err(e) => return Err(protocol("row_refused")(e)),
    };
    let op = a.check_epoch(hotkey, epoch)?;
    let label = a.summary().cohort_label;
    let work = WeightWork {
        manifest_hash: a.verified.target_digest.0,
        cohort: label,
        revision: 0,
        mechanism: MechId(a.verified.chain.mechanism),
        tempo: Some(epoch),
        v2_target: Some(a.verified.target_digest.0),
    };
    // The canonical row sums to 65,535; it is committed max-upscaled, the
    // form rtao's preparation and the chain's reveal both keep.
    let chain_entries = row.chain_values();
    let input: BTreeMap<Uid, f64> = chain_entries.iter().map(|&(uid, w)| (Uid(uid), f64::from(w))).collect();
    let _ = op;
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
/// [`VerifiedTarget::active`] (the launch burn row, or a target a checked
/// closure activated) and only ever moves forward, so a superseded bundle or
/// the launch authorization after the first activation is refused.
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
}

pub const HELD_TARGET_SCHEMA: &str = "slop_ninja.validator_held_target/v1";

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
        }
    }

    /// The record bound to `keys`.
    pub fn bound_to(mut self, keys: &TrustedKeys) -> Self {
        self.authority = Some(authority_digest(keys));
        self
    }

    pub fn restore(&self, chain: &ChainBinding) -> Result<ActiveTarget, Refusal> {
        let bad = |what: &str| Refusal::new("held_target_invalid", what.to_owned());
        if self.schema != HELD_TARGET_SCHEMA {
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
