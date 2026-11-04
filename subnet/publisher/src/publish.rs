//! One publisher run over the three buckets: the private receipts bucket
//! (cohort objects and authorizations), the private eval bucket (artifact
//! objects and admission records) and the public-candidate bucket.
//!
//! The public record (decision U2's restart) is the set of closed cohorts
//! whose manifest opened at or after `PUBLISH_FROM_EPOCH` (and, when
//! `RECORD_LABEL_PREFIX` is set, whose label starts with it). Chain epochs
//! only increase, so the boundary holds whatever numbering a restarted
//! coordinator uses. For each closed cohort not yet in the public index:
//!
//! 1. a label outside the prefix is skipped without a read;
//! 2. the manifest alone is opened; a cohort before the boundary, for another
//!    chain, or whose manifest does not open under the compiled trust roots
//!    is remembered and never read again;
//! 3. the in-record cohorts are verified in chain order, at most one to
//!    completion per run ([`verify::verify_cohort`]); the record starts at a
//!    cohort whose reference is `R_0` with no predecessor, and cohort numbers
//!    must order like their opening epochs. Cohorts are listed strictly in
//!    order: a cohort that is incomplete or unreadable for now (a store
//!    error, an object not stored yet, a closure or manifest that does not
//!    answer during the scan) holds every later one, so a later cohort, and
//!    its target and authorization, never appear before an earlier one. A
//!    packaged in-record cohort with no closure yet holds every cohort opened
//!    after it the same way, until a closure follows or something says none
//!    will: the coordinator's signed abandonment record ([`ABANDONMENT`],
//!    verified for its signature, label and chain), or its retirement marker
//!    for that cohort (written only after a closure or an abandonment, so
//!    with no closure it means the cohort was abandoned; due after two later
//!    completions, which bounds the hold);
//! 4. the cohort's allow-listed summary ([`crate::summary`]) is written, and
//!    read back through the allow-list check ([`crate::allowlist`]); none of
//!    the coordinator's signed records, indexes, components or receipts is
//!    copied: they stay coordinator-private;
//! 5. artifacts are published default-deny ([`static_withhold`], then the
//!    entrant's admission record, [`EntrantFacts::admitted_by`]);
//! 6. the publication record and the index entry are written, then, for the
//!    newest cohort, the leaderboard, the page and the reference pointers.
//!
//! A refused cohort is remembered with the publisher build that refused it
//! and is not read again until a new build (a new version or new trust
//! roots); its alert stays raised with the full reason. The public index
//! records it as a content-free entry (`CohortIndex::refused`: label,
//! number, the object and kind of failure ([`public_reason`]), build), so
//! the gap is visible and later cohorts are listed after it; a later build
//! that accepts it lists it in its place, also when it starts the record.
//! A refused cohort that starts the record starts it for later cohorts;
//! before the record started, one that cannot start it (or whose manifest
//! did not open) starts nothing ([`record_start`]).
//! Artifacts not copied in one run stay pending and are retried by later
//! runs. The current validator authorization, its renewal, the revocation
//! set and the coordinator's signed row are copied as soon as they verify
//! ([`Run::authorizations`]): validators commit that row and need no cohort
//! record.
//!
//! No detailed receipt is ever published (the receipts, the receipt index,
//! the chunk maps and the bundles are coordinator-private audit evidence),
//! so the public host has no retention rule. Nothing is deleted except the
//! leaderboard and reference pointers at a record restart and the v1
//! layout's objects at the index migration, and nothing is
//! written to the private buckets.

use std::cell::Cell;
use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use slop_ninja_admission_wire::json::Chain;
use slop_ninja_protocol::submission::SubmissionUpload;
use slop_ninja_protocol::{
    AccountId, CalculationTargetV2, CoordinatorRow, Digest, EntrantScore, LaunchAuthorization,
    Nonce, Record, ReferenceModel, ReferenceSet, RewardIntentAuthorization, RewardIntentRenewal,
    RewardIntentRevocation, Signed,
};

use crate::arena::{self, Budget};
use crate::config::{Config, TRUST_FILE};
use crate::index::{
    self, ArtifactEntry, ArtifactState, CohortIndex, Publication, ReferencePointer,
};
use crate::layout::{self, Cache};
use crate::leaderboard::{self, LeaderboardDoc};
use crate::store::{PublicStore, Put, Store, StoreError};
use crate::summary;
use crate::verify::{self, ManifestCheck, Options, Refusal, VerifiedCohort};

pub const PUBLISHER_ID: &str = concat!("slop-ninja-publisher/", env!("CARGO_PKG_VERSION"));

/// This build: its version and the trust roots compiled into it. Refusals
/// and unverifiable manifests are remembered per build, so a new version or
/// a rotation (decision D20) retries them.
pub fn build_id() -> String {
    let trust = slop_ninja_protocol::object_sha256(TRUST_FILE).hex();
    format!("{PUBLISHER_ID}+trust-{}", &trust[..16])
}

pub struct Buckets<'a, S> {
    pub receipts: &'a S,
    pub eval: &'a S,
    /// The public bucket, writable only through the allow-list.
    pub public: PublicStore<'a, S>,
}

/// The digests of one initial reference.
#[derive(Clone, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct ReferenceDigests {
    pub content: String,
    pub artifact: String,
}

impl ReferenceDigests {
    pub fn of(r: &ReferenceModel) -> Self {
        Self {
            content: r.content.hex(),
            artifact: r.artifact.hex(),
        }
    }
}

/// A closed cohort outside the public record.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Outside {
    /// Its manifest's opening epoch, when that placed it before the record.
    pub opened_epoch: Option<u64>,
    pub reason: String,
}

/// A cohort this build will not read again.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Remembered {
    pub build: String,
    pub reason: String,
    /// Its manifest's cohort number and opening epoch, when that opened (a
    /// refusal's public index entry carries them).
    #[serde(default)]
    pub cohort: Option<u64>,
    #[serde(default)]
    pub opened_epoch: Option<u64>,
    /// Its manifest starts the record (reference index 0, no predecessor);
    /// `false` when the manifest did not open.
    #[serde(default)]
    pub starts_record: bool,
    /// The reason the public index records: the object and the kind of
    /// failure only ([`public_reason`]); `reason`, in full, stays in the
    /// alert detail.
    #[serde(default)]
    pub public: Option<String>,
}

/// A refusal's public, content-free reason: the object it names and the
/// kind of failure, without the free-form detail (protocol error text,
/// memory budget figures), which stays in the alert.
pub fn public_reason(r: &Refusal) -> String {
    match r {
        Refusal::Missing(rel) => format!("{rel}: not stored"),
        Refusal::Store(rel, _) => format!("{rel}: the store did not answer"),
        Refusal::Budget(rel, _) => format!("{rel}: exceeds the run's memory budget"),
        Refusal::Invalid(rel, _) => format!("{rel}: does not verify"),
    }
}

/// The public reason of a numbering conflict ([`numbering`]).
pub const NUMBERING_CONFLICT: &str =
    "cohort number and opening epoch conflict with a listed cohort";

/// The coordinator's signed record that it abandoned a cohort, in the
/// cohort's private directory: no closure follows. The name and read bound
/// come from the shared contract (`slop_ninja_admission_wire::receipts`); a
/// record that verifies (signature under the compiled trust roots, the
/// cohort's label, this chain) stops a packaged cohort holding later ones,
/// and one that does not holds as if absent. The coordinator's retirement
/// marker for the cohort ([`RETIRED_MARKER`]) with no closure beside it
/// releases the hold the same way ([`Run::pending_closure`]).
pub use slop_ninja_admission_wire::receipts::ABANDONMENT_RECORD as ABANDONMENT;

/// Small durable memory kept by the Worker between runs.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Memo {
    /// Closed cohorts outside the record: label to why. Never read again
    /// while the boundary stands.
    pub outside_record: BTreeMap<String, Outside>,
    /// Closed cohorts whose manifest does not open under this build's trust
    /// roots: label to the protocol's reason.
    pub unverifiable: BTreeMap<String, Remembered>,
    /// In-record cohorts this build refused: label to the reason.
    pub refused: BTreeMap<String, Remembered>,
    /// Every initial reference (`R_0` and its replacement) any opened
    /// manifest named, inside or outside the record.
    pub references: BTreeSet<ReferenceDigests>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Report {
    pub published: Vec<String>,
    pub artifacts_copied: u32,
    /// Refused cohorts, artifacts or authorizations: subject, reason. Raises
    /// an alert.
    pub refused: Vec<(String, String)>,
    /// Incomplete work: subject, reason. Alerts after a tempo.
    pub pending: Vec<(String, String)>,
    /// Closed cohorts whose manifest does not open: one aggregated alert.
    pub unverifiable: Vec<String>,
    /// Entries of the copied revocation set that do not open under this
    /// build's trust roots (copied unchanged; validators skip them): one
    /// alert.
    pub revocations_unopened: u32,
    /// Closed cohorts outside the record seen this run.
    pub outside: u32,
    /// The stored index was layout v1's (it listed signed records and
    /// receipt counts) and was replaced by an empty v2 index this run, which
    /// lists every in-record cohort again from its private records.
    pub index_migrated: bool,
    /// Cohorts recorded this run as retired before publication: their
    /// detailed receipts were deleted under the N+2 rule before they were
    /// copied (`CohortIndex::retired_unpublished`). A final state: never
    /// pending, never retried.
    pub retired_unpublished: Vec<String>,
    pub publication_disabled: bool,
    pub index_written: bool,
    pub authorization_written: bool,
    /// A new public record started this run; the previous one was moved
    /// under `v1/records/<its first label>/`.
    pub record_restarted: Option<String>,
    /// Disagreements between the coordinator's retirement markers and a
    /// public retention reading (`retention_stalled` for `public_receipts`).
    /// Always empty: no detailed receipt is published, so the public host
    /// has no retention to cross-check; a run that reaches its end reports
    /// that (`retention_checked`), which resolves an earlier build's alert.
    pub retention_mismatch: Vec<String>,
    /// The pass ran to its end ([`crate::alerts::retention_after_pass`]).
    pub retention_checked: bool,
}

/// Why an artifact is or is not published.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Decision {
    Withhold(&'static str),
    Wait(&'static str),
    Copy(String),
}

pub const WITHHELD_OPERATOR: &str = "an operator or synthetic hotkey";
pub const WITHHELD_LISTED: &str = "a listed reference digest: references are published as reference packages, not as entrant artifacts";
pub const WITHHELD_INITIAL: &str = "an initial reference R_0 (or its replacement): published as a reference package, not as an entrant artifact";
pub const WITHHELD_COPY: &str =
    "failed the public-copy gate: a copy of a reference or of a published artifact";
pub const WITHHELD_CLUSTER: &str = "in a near-copy cluster with a withheld artifact";
pub const WITHHELD_NOT_ADMITTED: &str = "no outside admission record: not uploaded through the admission route under the submission terms";
pub const WITHHELD_ADMISSION: &str = "the admission record does not match the entrant";
pub const WAIT_LAYOUT: &str = "the admission object layout is not configured";
pub const WAIT_SOURCE: &str = "the artifact object is not in the eval bucket";
pub const WAIT_BUDGET: &str = "the run reached its wall budget";
pub const WAIT_STORE: &str = "the eval bucket did not answer";
pub const WAIT_SUMMARY: &str =
    "the cohort summary that names its near-copy clusters is not readable";

/// The withheld reasons that withhold an artifact's near-copy cluster: the
/// static checks, and the cluster reason itself (its cluster already holds
/// a statically withheld member).
const CLUSTER_WITHHOLDING: [&str; 5] = [
    WITHHELD_OPERATOR,
    WITHHELD_LISTED,
    WITHHELD_INITIAL,
    WITHHELD_COPY,
    WITHHELD_CLUSTER,
];

/// For each entrant, whether its near-copy cluster (`cluster[i]`, any label
/// every member shares; `None` outside every cluster) has a member marked in
/// `withheld`. The rule reads only membership, never which member is the
/// cluster's head, so a cluster is withheld whole.
fn in_withheld_cluster<K: PartialEq>(cluster: &[Option<K>], withheld: &[bool]) -> Vec<bool> {
    cluster
        .iter()
        .map(|c| {
            c.as_ref().is_some_and(|c| {
                cluster
                    .iter()
                    .zip(withheld)
                    .any(|(x, &w)| w && x.as_ref() == Some(c))
            })
        })
        .collect()
}

/// What an artifact decision needs to know about one entrant, from the
/// verified package and closure or from a publication record.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EntrantFacts {
    pub cohort: u64,
    pub hotkey: AccountId,
    pub registration_block: u64,
    pub content: Digest,
    pub artifact: Digest,
    pub commitment: Digest,
    /// The closure's opening nonce for this entrant.
    pub nonce: Option<Nonce>,
}

fn digest_hex(s: &str) -> Option<Digest> {
    let b: [u8; 32] = hex::decode(s).ok()?.try_into().ok()?;
    Some(Digest(b))
}

impl EntrantFacts {
    pub fn of(e: &EntrantScore, v: &VerifiedCohort) -> Self {
        Self {
            cohort: v.manifest.cohort,
            hotkey: e.identity.hotkey,
            registration_block: e.identity.registration_block,
            content: e.content,
            artifact: e.artifact,
            commitment: e.commitment,
            nonce: v
                .closure
                .openings
                .iter()
                .find(|o| {
                    o.hotkey == e.identity.hotkey
                        && o.registration_block == e.identity.registration_block
                })
                .map(|o| o.nonce),
        }
    }

    pub fn from_entry(a: &ArtifactEntry, cohort: u64) -> Option<Self> {
        Some(Self {
            cohort,
            hotkey: AccountId(digest_hex(&a.hotkey_hex)?.0),
            registration_block: a.registration_block,
            content: digest_hex(&a.content)?,
            artifact: digest_hex(&a.sha256)?,
            commitment: digest_hex(&a.commitment)?,
            nonce: digest_hex(&a.nonce).map(|d| Nonce(d.0)),
        })
    }

    /// The publication record's entry, still undecided.
    fn entry(&self, uid: u16) -> ArtifactEntry {
        ArtifactEntry {
            uid,
            hotkey: crate::ss58::encode(&self.hotkey),
            sha256: self.artifact.hex(),
            content: self.content.hex(),
            state: ArtifactState::Pending,
            path: None,
            reason: None,
            hotkey_hex: hex::encode(self.hotkey.0),
            registration_block: self.registration_block,
            commitment: self.commitment.hex(),
            nonce: self.nonce.map(|n| hex::encode(n.0)).unwrap_or_default(),
        }
    }

    /// Whether a stored `SubmissionUpload` is this entrant's admitted
    /// upload: this chain and cohort, the entrant's hotkey and registration,
    /// its committed content, commitment and artifact object, and the nonce
    /// the signed closure published.
    pub fn admitted_by(&self, u: &SubmissionUpload, cfg: &Config) -> bool {
        let o = &u.opening;
        o.chain == cfg.chain
            && o.cohort == self.cohort
            && o.hotkey == self.hotkey
            && o.registration_block == self.registration_block
            && o.content == self.content
            && u.commitment == self.commitment
            && u.artifact_sha256 == self.artifact
            && self.nonce.is_some_and(|n| n == o.nonce)
    }
}

/// The checks that need no read, in order: an operator or synthetic hotkey,
/// a listed reference digest, an initial reference any manifest named (or the
/// current one's), and, at the first decision, a public-copy gate failure.
/// Returns why the artifact is withheld, if it is.
pub fn static_withhold(
    f: &EntrantFacts,
    score: Option<&EntrantScore>,
    cfg: &Config,
    references: &BTreeSet<ReferenceDigests>,
) -> Option<&'static str> {
    if cfg.operator_hotkeys.contains(&f.hotkey) {
        return Some(WITHHELD_OPERATOR);
    }
    if cfg.private_digests.contains(&f.content) || cfg.private_digests.contains(&f.artifact) {
        return Some(WITHHELD_LISTED);
    }
    let (content, artifact) = (f.content.hex(), f.artifact.hex());
    if references
        .iter()
        .any(|r| r.content == content || r.artifact == artifact)
    {
        return Some(WITHHELD_INITIAL);
    }
    if score.is_some_and(|e| e.public_copy().is_some()) {
        return Some(WITHHELD_COPY);
    }
    None
}

/// The record starts at `R_0`: its first cohort's reference has index 0 and
/// no predecessor. A later cohort waits until such a cohort is listed,
/// recorded as retired before publication (its detailed receipts deleted
/// under the coordinator's N+2 rule before they were copied: it will never
/// be listed, and the index explains the gap), or recorded as refused (the
/// index shows the gap; a later build that accepts it lists it in its
/// place). A refused cohort whose manifest did not open is not known to
/// start the record, so later cohorts still wait for one that does.
pub fn record_start(idx: &CohortIndex, v: &VerifiedCohort) -> Result<(), String> {
    let refs = &v.manifest.references;
    let n = v.manifest.cohort;
    let earlier_start = idx
        .retired_unpublished
        .iter()
        .any(|r| r.starts_record && r.cohort < n)
        || idx
            .refused
            .iter()
            .any(|r| r.starts_record && r.cohort.is_some_and(|c| c < n));
    if idx.cohorts.is_empty() && !earlier_start && !starts_record(refs) {
        return Err(format!(
            "waiting for the record's first cohort: this one's reference has index {}",
            refs.index
        ));
    }
    Ok(())
}

/// Whether a cohort with these references can start the record: `R_0`, no
/// predecessor.
pub fn starts_record(refs: &ReferenceSet) -> bool {
    refs.index == 0 && refs.predecessor.is_none()
}

/// Whether cohort `cohort` (opened at `opened_epoch`) comes before the
/// record's first listed cohort.
fn precedes_first(idx: &CohortIndex, cohort: u64, opened_epoch: u64) -> bool {
    idx.cohorts
        .first()
        .is_some_and(|f| cohort < f.cohort && opened_epoch < f.opened_epoch)
}

/// Cohort numbers must order like opening epochs across the record (one
/// authority's numbering), with no number listed twice.
pub fn numbering(idx: &CohortIndex, v: &VerifiedCohort) -> Result<(), String> {
    let (n, e) = (v.manifest.cohort, v.manifest.opened_epoch);
    for c in &idx.cohorts {
        if c.cohort == n || (c.cohort < n) != (c.opened_epoch < e) {
            return Err(format!(
                "cohort {n} opened at epoch {e} conflicts with listed cohort {} opened at epoch {}",
                c.cohort, c.opened_epoch
            ));
        }
    }
    Ok(())
}

fn put_meta(key: &str, cache: Cache) -> Put<'_> {
    Put {
        content_type: layout::content_type(key),
        cache,
        sha256: None,
    }
}

/// Writes a public JSON document; the public store refuses it unless the
/// allow-list accepts it ([`PublicStore`]).
async fn put_json<S: Store, T: Serialize>(
    store: PublicStore<'_, S>,
    key: &str,
    value: &T,
    cache: Cache,
) -> Result<(), StoreError> {
    let bytes = serde_json::to_vec_pretty(value).map_err(|_| StoreError::Unavailable("json"))?;
    store.put(key, bytes, &put_meta(key, cache)).await
}

/// Writes a write-once public object (an archived record index); the public
/// store refuses it unless the allow-list accepts it.
async fn write_public_once<S: Store>(
    store: PublicStore<'_, S>,
    key: &str,
    bytes: Vec<u8>,
    put: &Put<'_>,
) -> Result<(), StoreError> {
    store.put(key, bytes, put).await
}

async fn get_json<S: Store, T: for<'d> Deserialize<'d>>(
    store: PublicStore<'_, S>,
    key: &str,
    budget: &Budget,
) -> Result<Option<T>, String> {
    let Some(bytes) = store
        .get(key, budget.remaining())
        .await
        .map_err(|e| e.to_string())?
    else {
        return Ok(None);
    };
    let _hold = budget
        .reserve(bytes.len() as u64)
        .map_err(|_| "budget".to_owned())?;
    serde_json::from_slice(&bytes)
        .map(Some)
        .map_err(|_| format!("{key} is not readable"))
}

/// Writes `bytes` at public `key` unless the store already holds them (by
/// SHA-256), once the allow-list accepts them (checked first, so bytes it
/// refuses are refused even when the store already holds them).
async fn put_if_changed<S: Store>(
    store: PublicStore<'_, S>,
    key: &str,
    bytes: Vec<u8>,
    cache: Cache,
) -> Result<bool, StoreError> {
    crate::allowlist::check_object(key, &bytes).map_err(|_| StoreError::NotAllowed)?;
    let digest = slop_ninja_protocol::object_sha256(&bytes);
    if let Some(meta) = store.head(key).await?
        && meta.size == bytes.len() as u64
        && meta.sha256 == Some(digest)
    {
        return Ok(false);
    }
    store
        .put(
            key,
            bytes,
            &Put {
                content_type: layout::content_type(key),
                cache,
                sha256: Some(&digest),
            },
        )
        .await?;
    Ok(true)
}

/// Copies one verified object (`bytes`, as read and checked) to the public
/// bucket unless it already holds it; the public store checks the bytes
/// against the allow-list and binds the copy to their digest.
async fn copy_checked<S: Store>(
    from: &S,
    to: PublicStore<'_, S>,
    src: &str,
    dst: &str,
    bytes: &[u8],
    cache: Cache,
) -> Result<bool, StoreError> {
    let digest = slop_ninja_protocol::object_sha256(bytes);
    crate::allowlist::check_object(dst, bytes).map_err(|_| StoreError::NotAllowed)?;
    if let Some(meta) = to.head(dst).await?
        && meta.size == bytes.len() as u64
        && meta.sha256 == Some(digest)
    {
        return Ok(true);
    }
    let put = Put {
        content_type: layout::content_type(dst),
        cache,
        sha256: Some(&digest),
    };
    to.copy_checked_from(from, src, dst, bytes, &put).await
}

/// Copies one artifact object, or says why not.
async fn copy_artifact<S: Store>(
    b: &Buckets<'_, S>,
    src: &str,
    sha256: &Digest,
) -> Result<ArtifactState, (ArtifactState, String, bool)> {
    let dst = layout::artifact(sha256);
    let unavailable = |e: StoreError| (ArtifactState::Pending, e.to_string(), false);
    if let Some(meta) = b.public.head(&dst).await.map_err(unavailable)?
        && meta.sha256 == Some(*sha256)
    {
        return Ok(ArtifactState::Published);
    }
    let Some(meta) = b.eval.head(src).await.map_err(unavailable)? else {
        return Err((ArtifactState::Pending, WAIT_SOURCE.into(), false));
    };
    let put = Put {
        content_type: "application/octet-stream",
        cache: Cache::Immutable,
        sha256: Some(sha256),
    };
    match b
        .public
        .copy_artifact_from(b.eval, src, &dst, meta.size, &put)
        .await
    {
        Ok(true) => Ok(ArtifactState::Published),
        Ok(false) => Err((ArtifactState::Pending, WAIT_SOURCE.into(), false)),
        Err(StoreError::Integrity) => Err((
            ArtifactState::Pending,
            "the stored object differs from the package's SHA-256".into(),
            true,
        )),
        Err(e) => Err(unavailable(e)),
    }
}

/// Renews the run's lease between steps.
#[allow(async_fn_in_trait)]
pub trait Keepalive {
    /// `false` when the lease was lost: the run stops starting work.
    async fn renew(&self) -> bool;
}

/// No lease (native tests and tools).
pub struct NoLease;

impl Keepalive for NoLease {
    async fn renew(&self) -> bool {
        true
    }
}

/// `v1/authorizations/current.json` and its private source: the
/// authorization that lets validators commit now, any renew-by hint refresh
/// and the revocation set (the latest signed revocation of every revoked
/// target, concatenated), each a content-addressed object. The
/// coordinator writes the private one with the same type.
pub use slop_ninja_admission_wire::json::{AuthorizationPointer, ObjectRef};

pub const AUTHORIZATION_SCHEMA: &str =
    slop_ninja_admission_wire::json::AUTHORIZATION_POINTER_SCHEMA;

/// The coordinator's marker, in a private cohort directory, that the
/// cohort's detailed receipts were deleted under the N+2 rule, and its
/// schema: the contract both crates import
/// (`slop_ninja_admission_wire::receipts`).
pub use slop_ninja_admission_wire::receipts::{
    RETIRED_MARKER, RETIRED_MARKER_SCHEMA, RetiredMarker,
};

pub struct Run<'a, S, K> {
    pub buckets: Buckets<'a, S>,
    pub cfg: &'a Config,
    pub budget: &'a Budget,
    /// Milliseconds since the run started.
    pub elapsed_ms: &'a dyn Fn() -> u64,
    pub keepalive: &'a K,
    /// Set once the lease is lost: no further step starts.
    pub stopped: Cell<bool>,
}

impl<S: Store, K: Keepalive> Run<'_, S, K> {
    fn opts(&self) -> Options<'_> {
        // Receipts are coordinator-private: the publisher neither reads nor
        // publishes them.
        Options {
            keys: &self.cfg.keys,
            chain: &self.cfg.chain,
            receipts: false,
        }
    }

    fn over_budget(&self) -> bool {
        self.stopped.get() || (self.elapsed_ms)() >= self.cfg.run_wall_budget_ms
    }

    /// Whether a new step may start: within the wall budget, lease renewed.
    async fn may_continue(&self) -> bool {
        if self.over_budget() {
            return false;
        }
        if !self.keepalive.renew().await {
            self.stopped.set(true);
            return false;
        }
        true
    }

    /// Publishes what is due. Returns the index as it now stands.
    pub async fn cohorts(
        &self,
        memo: &mut Memo,
        report: &mut Report,
    ) -> Result<CohortIndex, String> {
        let b = &self.buckets;
        let chain = &self.cfg.chain;
        // The authorization pointer, its objects, the revocation set and
        // the coordinator's row first, on their own: validators need only
        // these, so no cohort-phase step below (the trust file, the page,
        // the index and its migration, a record change, a cohort listing)
        // can hold a new row or revocation back, whatever it fails with.
        self.authorizations(report).await;
        put_if_changed(b.public, layout::TRUST, TRUST_FILE.to_vec(), Cache::Index)
            .await
            .map_err(|e| format!("trust file: {e}"))?;
        if b.public
            .head(layout::SITE_INDEX)
            .await
            .map_err(|e| e.to_string())?
            .is_none()
        {
            self.write_site(None).await.map_err(|e| e.to_string())?;
        }
        let mut idx =
            match get_json::<S, serde_json::Value>(b.public, layout::COHORT_INDEX, self.budget)
                .await?
            {
                Some(v)
                    if v.get("schema").and_then(|s| s.as_str()) == Some(index::INDEX_SCHEMA) =>
                {
                    serde_json::from_value::<CohortIndex>(v)
                        .map_err(|_| "the public index is not readable".to_owned())?
                }
                Some(v)
                    if v.get("schema").and_then(|s| s.as_str()) == Some(index::INDEX_SCHEMA_V1) =>
                {
                    let i = index::migrate_v1(&v, chain)?;
                    // Layout v1's leaderboard, reference pointers and
                    // archived record indexes carry fields v2 never writes
                    // (scores, counts, record digests): they are removed and
                    // the page rewritten without a leaderboard before the v2
                    // index is written, so an interrupted migration repeats
                    // this step. Re-listing the cohorts writes the
                    // leaderboard, pointers and page again in v2 shape; the
                    // earlier records keep their entry in the index (prefix
                    // and initial references) without their archive.
                    for key in [
                        layout::LEADERBOARD,
                        layout::REFERENCE_CURRENT,
                        layout::REFERENCE_PREVIOUS,
                    ] {
                        b.public.delete(key).await.map_err(|e| e.to_string())?;
                    }
                    for record in &i.previous_records {
                        let archive =
                            layout::record_archive(&record.first_label, "cohorts/index.json");
                        b.public.delete(&archive).await.map_err(|e| e.to_string())?;
                    }
                    self.write_site(None).await.map_err(|e| e.to_string())?;
                    put_json(b.public, layout::COHORT_INDEX, &i, Cache::Index)
                        .await
                        .map_err(|e| e.to_string())?;
                    report.index_written = true;
                    report.index_migrated = true;
                    i
                }
                Some(_) => {
                    return Err(
                        "the public index has an unknown schema; refusing to overwrite it".into(),
                    );
                }
                None => {
                    let mut i = CohortIndex::empty(chain);
                    i.record_prefix = self.cfg.record_label_prefix.clone();
                    put_json(b.public, layout::COHORT_INDEX, &i, Cache::Index)
                        .await
                        .map_err(|e| e.to_string())?;
                    report.index_written = true;
                    i
                }
            };
        match index::record_change(&idx, self.cfg.record_label_prefix.as_deref()) {
            index::RecordChange::Same(None) => {}
            index::RecordChange::Same(Some(prefix)) => {
                idx.record_prefix = Some(prefix);
                put_json(b.public, layout::COHORT_INDEX, &idx, Cache::Index)
                    .await
                    .map_err(|e| e.to_string())?;
                report.index_written = true;
            }
            index::RecordChange::Restart => {
                idx = self.start_record(&idx, report).await?;
            }
            index::RecordChange::Refused(why) => return Err(why),
        }
        // What this build remembers; a new build or boundary reconsiders.
        let build = build_id();
        memo.refused.retain(|_, r| r.build == build);
        memo.unverifiable.retain(|_, r| r.build == build);
        // Only this record's cohorts: an earlier record's refusals never
        // reach this record's public `refused` list or its completions.
        if let Some(prefix) = &self.cfg.record_label_prefix {
            memo.refused.retain(|label, _| label.starts_with(prefix));
            memo.unverifiable
                .retain(|label, _| label.starts_with(prefix));
            memo.outside_record
                .retain(|label, _| label.starts_with(prefix));
        }
        if let Some(from) = self.cfg.publish_from_epoch {
            memo.outside_record
                .retain(|_, o| o.opened_epoch.is_none_or(|e| e < from));
            memo.refused
                .retain(|_, r| r.opened_epoch.is_none_or(|e| e >= from));
        }
        for (label, r) in &memo.refused {
            report.refused.push((label.clone(), r.reason.clone()));
        }
        report.unverifiable = memo.unverifiable.keys().cloned().collect();

        self.retry_artifacts(&mut idx, memo, report).await?;
        match self.cfg.publish_from_epoch {
            Some(from) => self.new_cohorts(from, &mut idx, memo, report).await?,
            None => report.publication_disabled = true,
        }
        // No public receipts: nothing to cross-check (`Report::retention_mismatch`).
        report.retention_checked = true;
        Ok(idx)
    }

    /// Starts a new public record (a chain-record restart): the listed
    /// record's index is copied under `v1/records/<its first label>/` and the
    /// live leaderboard and reference pointers are removed, then an empty
    /// index for the configured record is written and the site page
    /// rewritten without a leaderboard. Only the index is archived: it is
    /// built from typed fields and carries no prose, while the leaderboard
    /// and pointers carry wording of the build that wrote them, which a
    /// write-once archive would keep public for good; each cohort's own
    /// directory holds its signed records. The archive is also scanned for
    /// wording public text never carries ([`crate::redact::scan_wording`])
    /// before it is written. The earlier cohorts' own directories stay where
    /// they are (their labels never recur under another authority), listed
    /// only by the archived index. The new index records the earlier record
    /// (`previous_records`: its prefix, never adopted again, and its initial
    /// references, still withheld as entrant artifacts). An archive object is written
    /// once: an existing one with the same bytes is an interrupted run
    /// repeating the step, and one with other bytes is refused, so no earlier
    /// archive is ever replaced.
    async fn start_record(
        &self,
        old: &CohortIndex,
        report: &mut Report,
    ) -> Result<CohortIndex, String> {
        let p = self.buckets.public;
        let first = old
            .cohorts
            .first()
            .map(|c| c.label.clone())
            .unwrap_or_default();
        {
            let key = layout::COHORT_INDEX;
            let Some(bytes) = p
                .get(key, self.budget.remaining())
                .await
                .map_err(|e| e.to_string())?
            else {
                return Err("the public index vanished during a record restart".into());
            };
            let _hold = self
                .budget
                .reserve(bytes.len() as u64)
                .map_err(|_| "budget".to_owned())?;
            crate::redact::scan_wording(&bytes)
                .map_err(|why| format!("the index to archive carries {why}; nothing archived"))?;
            let rel = key.trim_start_matches("v1/");
            let archive = layout::record_archive(&first, rel);
            let held = p
                .get(&archive, self.budget.remaining())
                .await
                .map_err(|e| e.to_string())?;
            match held {
                Some(held) if held == bytes => {}
                Some(_) => {
                    return Err(format!(
                        "{archive} exists with other bytes; an earlier record is never replaced"
                    ));
                }
                None => {
                    let digest = slop_ninja_protocol::object_sha256(&bytes);
                    write_public_once(
                        p,
                        &archive,
                        bytes,
                        &Put {
                            content_type: layout::content_type(key),
                            cache: Cache::Immutable,
                            sha256: Some(&digest),
                        },
                    )
                    .await
                    .map_err(|e| e.to_string())?;
                }
            }
        }
        for key in [
            layout::LEADERBOARD,
            layout::REFERENCE_CURRENT,
            layout::REFERENCE_PREVIOUS,
        ] {
            p.delete(key).await.map_err(|e| e.to_string())?;
        }
        let mut idx = CohortIndex::empty(&self.cfg.chain);
        idx.record_prefix = self.cfg.record_label_prefix.clone();
        idx.previous_records = old.previous_records.clone();
        idx.previous_records.push(index::ArchivedRecord {
            prefix: old.record_prefix.clone(),
            first_label: first.clone(),
            initial_references: old.initial_references.clone(),
        });
        put_json(p, layout::COHORT_INDEX, &idx, Cache::Index)
            .await
            .map_err(|e| e.to_string())?;
        self.write_site(None).await.map_err(|e| e.to_string())?;
        report.index_written = true;
        report.record_restarted = Some(first);
        Ok(idx)
    }

    /// Remembers cohort `label` as refused by this build; `at` is its
    /// manifest's cohort number, opening epoch and whether it starts the
    /// record, when that opened, and
    /// `public` the content-free reason the public index records (`reason`,
    /// in full, raises the alert). The public index entry follows at the end
    /// of the pass ([`Self::sync_refused`]).
    fn refuse(
        &self,
        memo: &mut Memo,
        report: &mut Report,
        label: String,
        at: Option<(u64, u64, bool)>,
        reason: String,
        public: String,
    ) {
        memo.refused.insert(
            label.clone(),
            Remembered {
                build: build_id(),
                reason: reason.clone(),
                cohort: at.map(|a| a.0),
                opened_epoch: at.map(|a| a.1),
                starts_record: at.is_some_and(|a| a.2),
                public: Some(public),
            },
        );
        report.refused.push((label, reason));
    }

    /// Brings the public index's refused entries in line with what this
    /// build remembers, and drops the entries of cohorts this pass placed
    /// otherwise (`resolved`: listed elsewhere, retired, outside the record
    /// or unverifiable). Entries an earlier build wrote stay until this build
    /// reconsiders their cohort. Writes the index when anything changed. Run
    /// at the end of every pass, so a refusal remembered by a run that
    /// stopped early is still recorded by the next.
    async fn sync_refused(
        &self,
        idx: &mut CohortIndex,
        memo: &Memo,
        resolved: &[String],
        report: &mut Report,
    ) -> Result<(), String> {
        let before = idx.refused.clone();
        for label in resolved {
            idx.clear_refused(label);
        }
        for (label, r) in &memo.refused {
            if idx.contains(label) || idx.retired_unpublished.iter().any(|x| &x.label == label) {
                continue;
            }
            let entry = index::RefusedCohort {
                label: label.clone(),
                cohort: r.cohort,
                opened_epoch: r.opened_epoch,
                starts_record: r.starts_record,
                reason: r.public.clone().unwrap_or_else(|| "refused".into()),
                build: r.build.clone(),
            };
            if !idx.refused.contains(&entry) {
                idx.note_refused(entry);
            }
        }
        if idx.refused != before {
            put_json(self.buckets.public, layout::COHORT_INDEX, idx, Cache::Index)
                .await
                .map_err(|e| e.to_string())?;
            report.index_written = true;
        }
        Ok(())
    }

    /// Places every closed cohort not yet listed inside or outside the
    /// record, then verifies and publishes the in-record ones in chain order,
    /// at most one to completion per run, strictly in order (module docs):
    /// the first in-record cohort that is not final yet holds the rest.
    async fn new_cohorts(
        &self,
        from: u64,
        idx: &mut CohortIndex,
        memo: &mut Memo,
        report: &mut Report,
    ) -> Result<(), String> {
        let mut resolved = Vec::new();
        self.place_cohorts(from, idx, memo, report, &mut resolved)
            .await?;
        self.sync_refused(idx, memo, &resolved, report).await
    }

    async fn place_cohorts(
        &self,
        from: u64,
        idx: &mut CohortIndex,
        memo: &mut Memo,
        report: &mut Report,
        resolved: &mut Vec<String>,
    ) -> Result<(), String> {
        let b = &self.buckets;
        let dirs = b
            .receipts
            .list_dirs(layout::PRIVATE_COHORTS)
            .await
            .map_err(|e| e.to_string())?;
        // (opened epoch, label, cohort number, whether it starts the
        // record), from each opened manifest.
        let mut candidates: Vec<(u64, String, u64, bool)> = Vec::new();
        // A closed cohort whose closure or manifest did not answer: its place
        // in the order is unknown, so no cohort is listed this run.
        let mut held = false;
        // In-record cohorts packaged but not closed yet (opened epoch,
        // number, whether it starts the record): a closure the coordinator
        // has not written yet (paused, or waiting out its retry time while
        // later cohorts go on) holds every cohort opened after it.
        let mut unclosed: Vec<(u64, u64, bool)> = Vec::new();
        for dir in dirs {
            let label = dir
                .trim_start_matches(layout::PRIVATE_COHORTS)
                .trim_end_matches('/')
                .to_owned();
            if !layout::safe_label(&label)
                || idx.contains(&label)
                || idx.retired_unpublished.iter().any(|r| r.label == label)
            {
                continue;
            }
            if self
                .cfg
                .record_label_prefix
                .as_ref()
                .is_some_and(|p| !label.starts_with(p))
                || memo.outside_record.contains_key(&label)
            {
                report.outside += 1;
                continue;
            }
            if memo.unverifiable.contains_key(&label) || memo.refused.contains_key(&label) {
                continue;
            }
            let private = layout::private_cohort_dir(&label);
            match b
                .receipts
                .head(&layout::join(&private, verify::CLOSURE))
                .await
            {
                Ok(Some(_)) => {}
                Ok(None) => {
                    match self.pending_closure(&private, &label, from).await {
                        Ok(Some(at)) => unclosed.push(at),
                        Ok(None) => {}
                        Err(e) => {
                            report.pending.push((label, e));
                            held = true;
                        }
                    }
                    continue;
                }
                Err(e) => {
                    report.pending.push((label, e.to_string()));
                    held = true;
                    continue;
                }
            }
            let opened =
                verify::open_manifest(b.receipts, &private, &label, &self.opts(), self.budget)
                    .await;
            match opened {
                Ok(ManifestCheck::Opened(m)) => {
                    if let Some(r) = index::initial_reference(&m.references) {
                        memo.references.insert(ReferenceDigests::of(&r));
                    }
                    if m.opened_epoch < from {
                        memo.outside_record.insert(
                            label.clone(),
                            Outside {
                                opened_epoch: Some(m.opened_epoch),
                                reason: "opened before PUBLISH_FROM_EPOCH".into(),
                            },
                        );
                        resolved.push(label);
                        report.outside += 1;
                    } else {
                        let starts = starts_record(&m.references);
                        candidates.push((m.opened_epoch, label, m.cohort, starts));
                    }
                }
                Ok(ManifestCheck::Unverifiable(why)) => {
                    memo.unverifiable.insert(
                        label.clone(),
                        Remembered {
                            build: build_id(),
                            reason: format!("manifest.signed: {why}"),
                            cohort: None,
                            opened_epoch: None,
                            starts_record: false,
                            public: None,
                        },
                    );
                    resolved.push(label.clone());
                    report.unverifiable.push(label);
                }
                Ok(check @ (ManifestCheck::OtherChain | ManifestCheck::OtherKind)) => {
                    let reason = match check {
                        ManifestCheck::OtherChain => "another chain or subnet",
                        _ => "not a reward-v1 cohort manifest",
                    };
                    memo.outside_record.insert(
                        label.clone(),
                        Outside {
                            opened_epoch: None,
                            reason: reason.into(),
                        },
                    );
                    resolved.push(label);
                    report.outside += 1;
                }
                Ok(ManifestCheck::OtherLabel) => {
                    let why = "manifest.signed: label differs from its directory";
                    self.refuse(memo, report, label, None, why.into(), why.into())
                }
                Err(r) if r.retryable() => {
                    report.pending.push((label, r.to_string()));
                    held = true;
                }
                Err(r) => {
                    let public = public_reason(&r);
                    self.refuse(memo, report, label, None, r.to_string(), public)
                }
            }
        }
        if held {
            return Ok(());
        }
        candidates.sort();
        // The earliest unclosed cohort that can still be listed (one before
        // the record's first listed cohort that cannot start the record is
        // placed outside once closed, so it holds nothing).
        let first_unclosed = unclosed
            .iter()
            .filter(|(e, n, starts)| *starts || !precedes_first(idx, *n, *e))
            .min()
            .copied();
        for (opened_epoch, label, cohort, starts) in candidates {
            if !self.may_continue().await {
                break;
            }
            if let Some((_, n, _)) = first_unclosed.filter(|(e, _, _)| *e < opened_epoch) {
                report.pending.push((
                    label,
                    format!("waiting for the closure of earlier cohort {n}"),
                ));
                break;
            }
            let at = Some((cohort, opened_epoch, starts));
            if !starts && precedes_first(idx, cohort, opened_epoch) {
                // Before the record's first listed cohort (one that waited for
                // the record to start, which a later cohort started): never
                // listed after it. A cohort that starts the record itself
                // (one refused, then accepted by a later build) is listed in
                // its place instead. A number and epoch that disagree are a
                // numbering conflict ([`numbering`]).
                memo.outside_record.insert(
                    label.clone(),
                    Outside {
                        opened_epoch: None,
                        reason: "precedes the record's first listed cohort".into(),
                    },
                );
                resolved.push(label);
                report.outside += 1;
                continue;
            }
            let v = match verify::verify_cohort(
                b.receipts,
                &layout::private_cohort_dir(&label),
                &label,
                &self.opts(),
                self.budget,
            )
            .await
            {
                Ok(v) => v,
                Err(r @ Refusal::Missing(_)) => {
                    // Objects deleted under the coordinator's N+2 rule never
                    // return: its marker makes the cohort final here.
                    match self.retired_marker(&label).await {
                        Ok(Some(after)) => {
                            idx.retired_unpublished.push(index::RetiredCohort {
                                label: label.clone(),
                                cohort,
                                opened_epoch,
                                due_after_cohort: after,
                                starts_record: starts,
                            });
                            idx.clear_refused(&label);
                            put_json(b.public, layout::COHORT_INDEX, idx, Cache::Index)
                                .await
                                .map_err(|e| e.to_string())?;
                            report.index_written = true;
                            report.retired_unpublished.push(label);
                            continue;
                        }
                        Ok(None) => report.pending.push((label, r.to_string())),
                        Err(e) => report.pending.push((label, e.to_string())),
                    }
                    break;
                }
                Err(r) if r.retryable() => {
                    report.pending.push((label, r.to_string()));
                    break;
                }
                Err(r) => {
                    let public = public_reason(&r);
                    self.refuse(memo, report, label, at, r.to_string(), public);
                    break;
                }
            };
            if let Err(why) = record_start(idx, &v) {
                // Waiting for the record's first cohort: a later cohort that
                // starts the record is listed, and this one then precedes it.
                report.pending.push((label, why));
                continue;
            }
            if let Err(why) = numbering(idx, &v) {
                self.refuse(memo, report, label, at, why, NUMBERING_CONFLICT.into());
                break;
            }
            match self.publish_one(&v, idx, memo, report).await {
                Ok(()) => {
                    report.published.push(label);
                    break;
                }
                Err(r) if r.retryable() => {
                    report.pending.push((label, r.to_string()));
                    break;
                }
                Err(r) => {
                    let public = public_reason(&r);
                    self.refuse(memo, report, label, at, r.to_string(), public);
                    break;
                }
            }
        }
        Ok(())
    }

    /// Whether cohort `label`, which has no closure yet, is packaged and in
    /// the record, so its closure is only pending: its package is stored
    /// (a packaged cohort closes unless it is abandoned), nothing says it
    /// was abandoned, and its manifest opens with an opening epoch at or
    /// after `from`. Abandonment shows as the signed abandonment record
    /// ([`ABANDONMENT`], verified: [`Run::abandoned`]) or as the coordinator's retirement marker
    /// ([`RETIRED_MARKER`]): the coordinator writes that marker only for a
    /// closed cohort, after its closure, or for an abandoned one, so a
    /// marker with no closure means no closure follows. Returns its opening
    /// epoch, number and whether it starts the record. `Err` when a read did
    /// not answer, or the marker does not parse (its place is unknown); an
    /// absent or unverifiable manifest, or one outside the record, holds
    /// nothing.
    async fn pending_closure(
        &self,
        private: &str,
        label: &str,
        from: u64,
    ) -> Result<Option<(u64, u64, bool)>, String> {
        let r = self.buckets.receipts;
        let packaged = r
            .head(&layout::join(private, verify::PACKAGE))
            .await
            .map_err(|e| format!("{}: {e}", verify::PACKAGE))?
            .is_some();
        if !packaged || self.abandoned(private, label).await? {
            return Ok(None);
        }
        if self
            .retired_marker(label)
            .await
            .map_err(|e| e.to_string())?
            .is_some()
        {
            return Ok(None);
        }
        match verify::open_manifest(r, private, label, &self.opts(), self.budget).await {
            Ok(ManifestCheck::Opened(m)) if m.opened_epoch >= from => Ok(Some((
                m.opened_epoch,
                m.cohort,
                starts_record(&m.references),
            ))),
            Err(e @ Refusal::Store(..)) => Err(e.to_string()),
            _ => Ok(None),
        }
    }

    /// Whether the coordinator's signed abandonment record for cohort
    /// `label` ([`ABANDONMENT`]) is stored and verifies: read within its
    /// bound, signed by the package key under the compiled trust roots, for
    /// this label and chain. A record that is oversized or does not verify
    /// counts as no record (the cohort keeps holding, as before it was
    /// written); `Err` when the read did not answer.
    async fn abandoned(&self, private: &str, label: &str) -> Result<bool, String> {
        use slop_ninja_protocol::submission::CohortAbandonment;
        let key = layout::join(private, ABANDONMENT);
        let bytes = match self
            .buckets
            .receipts
            .get(
                &key,
                slop_ninja_admission_wire::receipts::ABANDONMENT_RECORD_BOUND,
            )
            .await
        {
            Ok(None) | Err(StoreError::TooLarge { .. }) => return Ok(false),
            Ok(Some(b)) => b,
            Err(e) => return Err(format!("{ABANDONMENT}: {e}")),
        };
        Ok(
            slop_ninja_protocol::Signed::<CohortAbandonment>::open(&bytes, &self.cfg.keys)
                .is_ok_and(|a| a.cohort_label == label && a.chain == self.cfg.chain),
        )
    }

    /// The coordinator's `receipts-retired.json` marker for cohort `label`:
    /// `Some(due_after_cohort)` when its detailed receipts were deleted under
    /// the N+2 rule (content free; read within its bound), `None` when there
    /// is no marker, so a missing object is still only not stored yet.
    async fn retired_marker(&self, label: &str) -> Result<Option<u64>, Refusal> {
        let key = layout::join(&layout::private_cohort_dir(label), RETIRED_MARKER);
        let bytes = match self
            .buckets
            .receipts
            .get(
                &key,
                slop_ninja_admission_wire::receipts::RETIRED_MARKER_BOUND,
            )
            .await
        {
            Ok(None) => return Ok(None),
            Ok(Some(b)) => b,
            Err(StoreError::TooLarge { .. }) => {
                return Err(Refusal::Invalid(key, "larger than its bound".into()));
            }
            Err(e) => return Err(Refusal::Store(key, e)),
        };
        let m = RetiredMarker::parse(&bytes, label).map_err(|_| {
            Refusal::Invalid(key, "unreadable, unknown schema or another label".into())
        })?;
        Ok(Some(m.due_after_cohort))
    }

    /// The initial references to withhold: every one any opened manifest
    /// named (the memo), every one a listed cohort named (the index), every
    /// one an earlier record's cohorts named (`previous_records`), and this
    /// cohort's own.
    fn withheld_references(
        &self,
        memo: &Memo,
        idx: &CohortIndex,
        v: Option<&VerifiedCohort>,
    ) -> BTreeSet<ReferenceDigests> {
        let mut out = memo.references.clone();
        out.extend(idx.withheld_initial_references().map(|r| ReferenceDigests {
            content: r.content.clone(),
            artifact: r.artifact.clone(),
        }));
        if let Some(r) = v.and_then(|v| index::initial_reference(&v.manifest.references)) {
            out.insert(ReferenceDigests::of(&r));
        }
        out
    }

    async fn publish_one(
        &self,
        v: &VerifiedCohort,
        idx: &mut CohortIndex,
        memo: &Memo,
        report: &mut Report,
    ) -> Result<(), Refusal> {
        let b = &self.buckets;
        let label = &v.label;
        let store_err = |e| Refusal::Store(label.clone(), e);
        // The allow-listed summary is the cohort's only public object. It is
        // checked against the allow-list before it is written and read back
        // after, so nothing outside the allow-list ever reaches the bucket.
        let doc = summary::build(v, Chain::from_binding(&self.cfg.chain));
        let summary_key = layout::join(&layout::cohort_dir(label), summary::SUMMARY);
        let bytes = serde_json::to_vec_pretty(&doc)
            .map_err(|_| Refusal::Store(label.clone(), StoreError::Unavailable("json")))?;
        crate::allowlist::check_object(&summary_key, &bytes)
            .map_err(|why| Refusal::Invalid(summary::SUMMARY.into(), why))?;
        let summary_sha = slop_ninja_protocol::object_sha256(&bytes);
        put_if_changed(b.public, &summary_key, bytes, Cache::Index)
            .await
            .map_err(store_err)?;
        match b.public.get(&summary_key, self.budget.remaining()).await {
            Ok(Some(back))
                if slop_ninja_protocol::object_sha256(&back) == summary_sha
                    && crate::allowlist::check_object(&summary_key, &back).is_ok() => {}
            Ok(_) => {
                return Err(Refusal::Invalid(
                    summary::SUMMARY.into(),
                    "differs after the write".into(),
                ));
            }
            Err(e) => return Err(store_err(e)),
        }
        // Artifacts, default-deny: the static checks, then the near-copy
        // cluster of a withheld artifact, then the admission record and the
        // copy.
        let references = self.withheld_references(memo, idx, Some(v));
        let entrants = &v.package.entrants;
        let facts: Vec<EntrantFacts> = entrants.iter().map(|e| EntrantFacts::of(e, v)).collect();
        let mut withheld: Vec<Option<&'static str>> = entrants
            .iter()
            .zip(&facts)
            .map(|(e, f)| static_withhold(f, Some(e), self.cfg, &references))
            .collect();
        // A cluster is withheld whole when any member is withheld by a
        // static check, whichever member is its head: the rule reads only the
        // cluster's membership (public in the summary) and the static checks,
        // never which member scored best, so the withheld reasons cannot
        // single out the head.
        // Retries apply the same rule (`retry_artifacts`).
        let statically: Vec<bool> = withheld.iter().map(Option::is_some).collect();
        let clusters: Vec<Option<AccountId>> = entrants.iter().map(|e| e.cluster_head).collect();
        for (why, hit) in withheld
            .iter_mut()
            .zip(in_withheld_cluster(&clusters, &statically))
        {
            if why.is_none() && hit {
                *why = Some(WITHHELD_CLUSTER);
            }
        }
        let mut artifacts = Vec::new();
        for ((e, f), why) in entrants.iter().zip(&facts).zip(&withheld) {
            let mut entry = f.entry(e.identity.uid);
            match why {
                Some(why) => {
                    entry.state = ArtifactState::Withheld;
                    entry.reason = Some((*why).into());
                }
                None => self.admit_and_copy(f, label, &mut entry, report).await,
            }
            artifacts.push(entry);
        }
        let publication = Publication {
            schema: index::PUBLICATION_SCHEMA.into(),
            cohort: v.manifest.cohort,
            label: label.clone(),
            publisher: PUBLISHER_ID.into(),
            summary: summary_sha.hex(),
            artifacts: artifacts.clone(),
        };
        put_json(
            b.public,
            &layout::publication(label),
            &publication,
            Cache::Index,
        )
        .await
        .map_err(store_err)?;
        let newest = idx.last_cohort().is_none_or(|c| v.manifest.cohort >= c);
        idx.note_initial_reference(v);
        idx.upsert(
            index::entry(v, &summary_sha.hex(), &artifacts),
            &self.cfg.chain,
        );
        idx.clear_refused(label);
        put_json(b.public, layout::COHORT_INDEX, idx, Cache::Index)
            .await
            .map_err(store_err)?;
        report.index_written = true;
        if newest {
            let (current, previous) = self.references(v).await.map_err(store_err)?;
            put_json(b.public, layout::REFERENCE_CURRENT, &current, Cache::Index)
                .await
                .map_err(store_err)?;
            if let Some(p) = &previous {
                put_json(b.public, layout::REFERENCE_PREVIOUS, p, Cache::Index)
                    .await
                    .map_err(store_err)?;
            }
            let doc = leaderboard::build(leaderboard::Inputs {
                summary: &doc,
                artifacts: &artifacts,
                active_target: &idx.active_target,
                current_reference: current,
                previous_reference: previous,
            });
            put_json(b.public, layout::LEADERBOARD, &doc, Cache::Index)
                .await
                .map_err(store_err)?;
            self.write_site(Some(&doc)).await.map_err(store_err)?;
        }
        Ok(())
    }

    /// The entrant's admission record decides, then the copy: no record
    /// withholds the artifact for good, a record that does not match it
    /// withholds it and raises an alert.
    async fn admit_and_copy(
        &self,
        f: &EntrantFacts,
        label: &str,
        entry: &mut ArtifactEntry,
        report: &mut Report,
    ) {
        let (Some(source), Some(record)) = (&self.cfg.artifact_source, &self.cfg.admission_record)
        else {
            entry.reason = Some(WAIT_LAYOUT.into());
            return;
        };
        let subject = format!("{label}/artifact-{}", &entry.sha256[..16]);
        if !self.may_continue().await {
            entry.reason = Some(WAIT_BUDGET.into());
            report.pending.push((subject, WAIT_BUDGET.into()));
            return;
        }
        let key = record
            .replace("{label}", label)
            .replace("{hotkey}", &hex::encode(f.hotkey.0));
        let admitted = match self
            .buckets
            .eval
            .get(&key, arena::ADMISSION_RECORD_BOUND)
            .await
        {
            Ok(None) => Err(WITHHELD_NOT_ADMITTED),
            Ok(Some(bytes)) => match SubmissionUpload::from_bytes(&bytes) {
                Ok(u) if f.admitted_by(&u, self.cfg) => Ok(()),
                _ => Err(WITHHELD_ADMISSION),
            },
            Err(StoreError::TooLarge { .. }) => Err(WITHHELD_ADMISSION),
            Err(e) => {
                entry.reason = Some(WAIT_STORE.into());
                report.pending.push((subject, e.to_string()));
                return;
            }
        };
        if let Err(why) = admitted {
            entry.state = ArtifactState::Withheld;
            entry.reason = Some(why.into());
            if why == WITHHELD_ADMISSION {
                report.refused.push((subject, why.into()));
            }
            return;
        }
        let src = source.replace("{sha256}", &entry.sha256);
        match copy_artifact(&self.buckets, &src, &f.artifact).await {
            Ok(state) => {
                entry.state = state;
                entry.path = Some(layout::artifact(&f.artifact));
                entry.reason = None;
                report.artifacts_copied += 1;
            }
            Err((state, why, refused)) => {
                entry.state = state;
                if refused {
                    report.refused.push((subject, why.clone()));
                } else {
                    report.pending.push((subject, why.clone()));
                }
                entry.reason = Some(why);
            }
        }
    }

    async fn references(
        &self,
        v: &VerifiedCohort,
    ) -> Result<(ReferencePointer, Option<ReferencePointer>), StoreError> {
        let refs = &v.manifest.references;
        let mut candidates = vec![refs.current.artifact];
        candidates.extend(refs.predecessor.map(|p| p.artifact));
        candidates.extend(
            v.package
                .promoted
                .and_then(|h| v.package.entrants.iter().find(|e| e.identity.hotkey == h))
                .map(|e| e.artifact),
        );
        let (mut published, mut staged) = (Vec::new(), Vec::new());
        for c in candidates {
            if let Some(meta) = self.buckets.public.head(&layout::artifact(&c)).await?
                && meta.sha256 == Some(c)
            {
                published.push(c.hex());
            } else if self.staged_package(&c).await? {
                staged.push(c.hex());
            }
        }
        Ok(index::references_after(
            v,
            |sha| published.iter().any(|p| p == sha),
            |sha| staged.iter().any(|p| p == sha),
        ))
    }

    /// Whether the public bucket holds a staged reference package for
    /// `artifact`: a parts manifest at [`layout::reference_package_manifest`],
    /// within the scorer's manifest bound, whose SHA-256 is `artifact`. The
    /// operator stages packages; the publisher only links them.
    async fn staged_package(&self, artifact: &Digest) -> Result<bool, StoreError> {
        let key = layout::reference_package_manifest(artifact);
        let bound = layout::REFERENCE_PACKAGE_MANIFEST_BOUND.min(self.budget.remaining());
        let bytes = match self.buckets.public.get(&key, bound).await {
            Ok(Some(b)) => b,
            Ok(None) | Err(StoreError::TooLarge { .. }) => return Ok(false),
            Err(e) => return Err(e),
        };
        let Ok(_hold) = self.budget.reserve(bytes.len() as u64) else {
            return Ok(false);
        };
        Ok(slop_ninja_protocol::object_sha256(&bytes) == *artifact)
    }

    async fn write_site(&self, doc: Option<&LeaderboardDoc>) -> Result<(), StoreError> {
        let p = self.buckets.public;
        put_if_changed(
            p,
            layout::SITE_STYLE,
            leaderboard::STYLE.as_bytes().to_vec(),
            Cache::Index,
        )
        .await?;
        put_if_changed(
            p,
            layout::SITE_INDEX,
            leaderboard::render(doc).into_bytes(),
            Cache::Snapshot,
        )
        .await?;
        Ok(())
    }

    /// Retries pending artifacts of published cohorts, newest first, with
    /// the static checks repeated (the configuration may list more since)
    /// and the cluster rule applied again, and refreshes the records that
    /// name them. A pending member of a cluster with a statically withheld
    /// member is withheld whole, by the membership the cohort's summary
    /// states; a member already published stays published (nothing recalls
    /// a public object).
    async fn retry_artifacts(
        &self,
        idx: &mut CohortIndex,
        memo: &Memo,
        report: &mut Report,
    ) -> Result<(), String> {
        if self.cfg.artifact_source.is_none() {
            // Nothing can be copied; the static checks run when it is set.
            return Ok(());
        }
        let b = &self.buckets;
        let references = self.withheld_references(memo, idx, None);
        let pending: Vec<String> = idx
            .cohorts
            .iter()
            .rev()
            .filter(|c| c.artifacts_pending > 0)
            .map(|c| c.label.clone())
            .collect();
        for label in pending {
            if self.over_budget() {
                break;
            }
            let Some(mut publication) =
                get_json::<S, Publication>(b.public, &layout::publication(&label), self.budget)
                    .await?
            else {
                report
                    .refused
                    .push((label, "publication record missing".into()));
                continue;
            };
            let cohort = publication.cohort;
            let before = publication.artifacts.clone();
            // As at the first decision: the static checks over every pending
            // artifact first, then the near-copy cluster of any artifact
            // withheld by them (now or on an earlier run), and only then the
            // admission record and the copy.
            let mut undecided: Vec<Option<EntrantFacts>> = Vec::new();
            for a in &mut publication.artifacts {
                let mut facts = None;
                if a.state == ArtifactState::Pending {
                    match EntrantFacts::from_entry(a, cohort) {
                        None => {
                            a.state = ArtifactState::Withheld;
                            a.reason = Some(WITHHELD_ADMISSION.into());
                        }
                        Some(f) => match static_withhold(&f, None, self.cfg, &references) {
                            Some(why) => {
                                a.state = ArtifactState::Withheld;
                                a.reason = Some(why.into());
                            }
                            None => facts = Some(f),
                        },
                    }
                }
                undecided.push(facts);
            }
            let withholding: Vec<bool> = publication
                .artifacts
                .iter()
                .map(|a| {
                    a.state == ArtifactState::Withheld
                        && a.reason
                            .as_deref()
                            .is_some_and(|r| CLUSTER_WITHHOLDING.contains(&r))
                })
                .collect();
            if withholding.iter().any(|&w| w) && undecided.iter().any(Option::is_some) {
                match self.summary_clusters(&label, &publication).await {
                    Ok(clusters) => {
                        let hits = in_withheld_cluster(&clusters, &withholding);
                        for ((a, f), hit) in publication
                            .artifacts
                            .iter_mut()
                            .zip(&mut undecided)
                            .zip(hits)
                        {
                            if hit && f.take().is_some() {
                                a.state = ArtifactState::Withheld;
                                a.reason = Some(WITHHELD_CLUSTER.into());
                            }
                        }
                    }
                    // Default-deny: without the membership nothing is copied.
                    Err(why) => {
                        for (a, f) in publication.artifacts.iter_mut().zip(&mut undecided) {
                            if f.take().is_some() {
                                a.reason = Some(WAIT_SUMMARY.into());
                                report.pending.push((
                                    format!("{label}/artifact-{}", &a.sha256[..16]),
                                    why.clone(),
                                ));
                            }
                        }
                    }
                }
            }
            for (a, f) in publication.artifacts.iter_mut().zip(&undecided) {
                if let Some(f) = f {
                    self.admit_and_copy(f, &label, a, report).await;
                }
            }
            if publication.artifacts == before {
                continue;
            }
            put_json(
                b.public,
                &layout::publication(&label),
                &publication,
                Cache::Index,
            )
            .await
            .map_err(|e| e.to_string())?;
            if let Some(entry) = idx.cohorts.iter_mut().find(|c| c.label == label) {
                let count = |s| {
                    publication
                        .artifacts
                        .iter()
                        .filter(|a| a.state == s)
                        .count() as u32
                };
                entry.artifacts_published = count(ArtifactState::Published);
                entry.artifacts_withheld = count(ArtifactState::Withheld);
                entry.artifacts_pending = count(ArtifactState::Pending);
            }
            put_json(b.public, layout::COHORT_INDEX, idx, Cache::Index)
                .await
                .map_err(|e| e.to_string())?;
            report.index_written = true;
            // The newest cohort's leaderboard and reference pointers name
            // these artifacts.
            if idx.cohorts.last().is_some_and(|c| c.label == label) {
                self.refresh_newest(&publication)
                    .await
                    .map_err(|e| e.to_string())?;
            }
        }
        Ok(())
    }

    /// The near-copy cluster of each of `publication`'s artifacts, by the
    /// label the cohort's public summary gives it (its first member in roster
    /// order; `None` outside every cluster). The summary must be the one the
    /// publication record names by digest; the membership it states is the
    /// package's, which the summary was rendered from.
    async fn summary_clusters(
        &self,
        label: &str,
        publication: &Publication,
    ) -> Result<Vec<Option<String>>, String> {
        let key = layout::join(&layout::cohort_dir(label), summary::SUMMARY);
        let bytes = self
            .buckets
            .public
            .get(&key, self.budget.remaining())
            .await
            .map_err(|e| e.to_string())?
            .ok_or_else(|| format!("{WAIT_SUMMARY}: missing"))?;
        let _hold = self
            .budget
            .reserve(bytes.len() as u64)
            .map_err(|_| "budget".to_owned())?;
        if slop_ninja_protocol::object_sha256(&bytes).hex() != publication.summary {
            return Err(format!(
                "{WAIT_SUMMARY}: differs from the publication record's digest"
            ));
        }
        let doc: summary::CohortSummary = serde_json::from_slice(&bytes)
            .map_err(|_| format!("{WAIT_SUMMARY}: not a {}", summary::SUMMARY_SCHEMA))?;
        publication
            .artifacts
            .iter()
            .map(|a| {
                doc.entrants
                    .iter()
                    .find(|e| e.hotkey_hex == a.hotkey_hex)
                    .map(|e| e.cluster.as_ref().map(|c| c.hotkey.clone()))
                    .ok_or_else(|| format!("{WAIT_SUMMARY}: uid {} is not listed", a.uid))
            })
            .collect()
    }

    async fn refresh_newest(&self, publication: &Publication) -> Result<(), StoreError> {
        let p = self.buckets.public;
        let Ok(Some(mut doc)) =
            get_json::<S, LeaderboardDoc>(p, layout::LEADERBOARD, self.budget).await
        else {
            return Ok(());
        };
        for row in &mut doc.entrants {
            if let Some(a) = publication
                .artifacts
                .iter()
                .find(|a| a.hotkey == row.entrant.hotkey)
            {
                row.artifact = Some(a.clone());
            }
        }
        let published = |sha: &str| {
            publication
                .artifacts
                .iter()
                .any(|a| a.sha256 == sha && a.state == ArtifactState::Published)
        };
        for pointer in
            std::iter::once(&mut doc.current_reference).chain(doc.previous_reference.as_mut())
        {
            if pointer.path.is_none()
                && published(&pointer.artifact)
                && let Some(sha) = digest_hex(&pointer.artifact)
            {
                pointer.path = Some(layout::artifact(&sha));
                pointer.package = None;
                pointer.licence = None;
                pointer.withheld = None;
            } else if pointer.path.is_none()
                && pointer.package.is_none()
                && let Some(sha) = digest_hex(&pointer.artifact)
                && self.staged_package(&sha).await?
            {
                pointer.link_package();
            }
        }
        put_json(p, layout::LEADERBOARD, &doc, Cache::Index).await?;
        put_json(
            p,
            layout::REFERENCE_CURRENT,
            &doc.current_reference,
            Cache::Index,
        )
        .await?;
        if let Some(prev) = &doc.previous_reference {
            put_json(p, layout::REFERENCE_PREVIOUS, prev, Cache::Index).await?;
        }
        self.write_site(Some(&doc)).await
    }

    /// Copies the current validator authorization, its renewal, the
    /// revocation set and the coordinator's signed row, then points
    /// `v1/authorizations/current.json` at them; reports problems.
    pub async fn authorizations(&self, report: &mut Report) {
        match self.copy_authorizations().await {
            Ok(copied) => {
                report.authorization_written = copied.written;
                report.revocations_unopened = copied.unopened;
                // A row that could not be copied is left out of the public
                // pointer (validators keep their installed row) and alerted:
                // refused, or pending while it may still appear.
                if let Some(r) = copied.row_dropped {
                    let entry = ("authorizations row".to_owned(), r.to_string());
                    if r.retryable() {
                        report.pending.push(entry);
                    } else {
                        report.refused.push(entry);
                    }
                }
            }
            Err(r) if r.retryable() => report
                .pending
                .push(("authorizations".into(), r.to_string())),
            Err(r) => report
                .refused
                .push(("authorizations".into(), r.to_string())),
        }
    }

    /// Reads one private authorization object named by `r`, checked against
    /// its digest and length.
    async fn authorization_object(&self, r: &ObjectRef) -> Result<(Digest, Vec<u8>), Refusal> {
        self.authorization_object_within(r, arena::AUTHORIZATION_OBJECT_BOUND)
            .await
    }

    /// [`Self::authorization_object`] within `bound`.
    async fn authorization_object_within(
        &self,
        r: &ObjectRef,
        bound: u64,
    ) -> Result<(Digest, Vec<u8>), Refusal> {
        let what = "authorization object";
        let sha = digest_hex(&r.sha256)
            .filter(|d| r.path == layout::private_authorization(d))
            .ok_or_else(|| Refusal::Invalid(what.into(), "not a content-addressed key".into()))?;
        let bytes = match self.buckets.receipts.get(&r.path, bound).await {
            Ok(Some(b)) => b,
            Ok(None) => return Err(Refusal::Missing(r.path.clone())),
            Err(StoreError::TooLarge { .. }) => {
                return Err(Refusal::Invalid(
                    what.into(),
                    "larger than its bound".into(),
                ));
            }
            Err(e) => return Err(Refusal::Store(r.path.clone(), e)),
        };
        if bytes.len() as u64 != r.bytes || slop_ninja_protocol::object_sha256(&bytes) != sha {
            return Err(Refusal::Invalid(
                what.into(),
                "digest or length differs from the pointer".into(),
            ));
        }
        Ok((sha, bytes))
    }

    /// Copies one verified authorization object (`bytes`, as read and
    /// checked) to its public key, once the allow-list accepts it.
    async fn copy_authorization(
        &self,
        r: &ObjectRef,
        sha: Digest,
        bytes: &[u8],
    ) -> Result<ObjectRef, Refusal> {
        let b = &self.buckets;
        let dst = layout::authorization(&sha);
        crate::allowlist::check_object(&dst, bytes)
            .map_err(|why| Refusal::Invalid(r.path.clone(), why))?;
        debug_assert_eq!(slop_ninja_protocol::object_sha256(bytes), sha);
        match copy_checked(b.receipts, b.public, &r.path, &dst, bytes, Cache::Immutable).await {
            Ok(true) => Ok(ObjectRef {
                path: dst,
                sha256: sha.hex(),
                bytes: r.bytes,
            }),
            Ok(false) => Err(Refusal::Missing(r.path.clone())),
            Err(StoreError::Integrity) => Err(Refusal::Invalid(
                r.path.clone(),
                "changed after verification".into(),
            )),
            Err(e) => Err(Refusal::Store(r.path.clone(), e)),
        }
    }

    async fn copy_authorizations(&self) -> Result<Copied, Refusal> {
        let b = &self.buckets;
        let mut unopened = 0u32;
        let pointer: AuthorizationPointer = match b
            .receipts
            .get(
                layout::PRIVATE_AUTHORIZATION_POINTER,
                arena::AUTHORIZATION_POINTER_BOUND,
            )
            .await
        {
            Ok(None) => return Ok(Copied::default()),
            Ok(Some(bytes)) => serde_json::from_slice(&bytes).map_err(|e| {
                Refusal::Invalid(layout::PRIVATE_AUTHORIZATION_POINTER.into(), e.to_string())
            })?,
            Err(StoreError::TooLarge { .. }) => {
                return Err(Refusal::Invalid(
                    layout::PRIVATE_AUTHORIZATION_POINTER.into(),
                    "larger than its bound".into(),
                ));
            }
            Err(e) => {
                return Err(Refusal::Store(
                    layout::PRIVATE_AUTHORIZATION_POINTER.into(),
                    e,
                ));
            }
        };
        let invalid = |why: &str| Refusal::Invalid("authorizations".into(), why.into());
        if pointer.schema != AUTHORIZATION_SCHEMA
            || pointer.chain != Chain::from_binding(&self.cfg.chain)
        {
            return Err(invalid("unknown schema, or another chain or subnet"));
        }
        let keys = &self.cfg.keys;
        let (auth_sha, auth_bytes) = self.authorization_object(&pointer.authorization).await?;
        let auth_digest = match pointer.kind.as_str() {
            "launch" => {
                let a = Signed::<LaunchAuthorization>::open(&auth_bytes, keys)
                    .map_err(|e| invalid(&format!("launch authorization: {e}")))?;
                let burn = CalculationTargetV2::launch_burn(self.cfg.chain.clone())
                    .digest()
                    .map_err(|e| invalid(&e.to_string()))?;
                if a.chain != self.cfg.chain || a.target != burn {
                    return Err(invalid("the launch authorization names another target"));
                }
                a.digest().map_err(|e| invalid(&e.to_string()))?
            }
            "cohort" => {
                let a = Signed::<RewardIntentAuthorization>::open(&auth_bytes, keys)
                    .map_err(|e| invalid(&format!("reward-intent authorization: {e}")))?;
                // Published at once: validators commit the coordinator's
                // signed row and need no record of the cohort.
                if a.chain != self.cfg.chain {
                    return Err(invalid("the authorization names another chain or subnet"));
                }
                a.digest().map_err(|e| invalid(&e.to_string()))?
            }
            _ => return Err(invalid("unknown authorization kind")),
        };
        let renewal = match &pointer.renewal {
            None => None,
            Some(r) => {
                let (sha, bytes) = self.authorization_object(r).await?;
                let renewal = Signed::<RewardIntentRenewal>::open(&bytes, keys)
                    .map_err(|e| invalid(&format!("renewal: {e}")))?;
                if renewal.authorization != auth_digest {
                    return Err(invalid("the renewal renews another authorization"));
                }
                Some((r, sha, bytes))
            }
        };
        // The revocation set is copied byte for byte whatever its entries
        // name: validators check each, journal those that verify and skip
        // the rest. Only a malformed set is refused (not whole records, a
        // digest or length that differs from the pointer, an entry that does
        // not decode or names another chain); an entry that does not open
        // under this build's trust roots (one signed under a root rotated on
        // the coordinator's side first) is counted and alerted, never a
        // reason to stop publishing newer authorizations. Its read bound is
        // its declared length (a whole number of fixed-length revocations),
        // reserved from the run's budget before it is read.
        let revocations = match &pointer.revocations {
            None => None,
            Some(r) => {
                let bound = slop_ninja_admission_wire::receipts::revocation_set_bound(
                    r.bytes,
                    &self.cfg.chain,
                )
                .map_err(|_| invalid("the revocation set is not whole revocations"))?;
                let _hold = self
                    .budget
                    .reserve(bound)
                    .map_err(|_| invalid("the revocation set exceeds the run's budget"))?;
                let (sha, bytes) = self.authorization_object_within(r, bound).await?;
                let entries = slop_ninja_admission_wire::receipts::revocation_set_entries(
                    &bytes,
                    &self.cfg.chain,
                )
                .map_err(|_| invalid("the revocation set is not whole revocations"))?;
                for entry in entries {
                    let signed = Signed::<RewardIntentRevocation>::from_bytes(entry)
                        .map_err(|e| invalid(&format!("revocation: {e}")))?;
                    if signed.record.chain != self.cfg.chain {
                        return Err(invalid("a revocation names another chain or subnet"));
                    }
                    if signed.verify(keys).is_err() {
                        unopened += 1;
                    }
                }
                Some((r, sha, bytes))
            }
        };
        // The coordinator's signed row: it opens under the trust roots,
        // names this chain and was committed under the authorization the
        // pointer names. Its read bound is the protocol's longest row. A row
        // that fails any of this, or cannot be read or copied, never holds
        // the pointer or its revocation set back: the pointer is published
        // without `row` (validators keep their installed row) and the
        // failure is reported for an alert.
        let mut row_dropped = None;
        let row = match &pointer.row {
            None => None,
            Some(r) => match self.checked_row(r, &auth_digest).await {
                Ok((sha, bytes)) => Some((r, sha, bytes)),
                Err(why) => {
                    row_dropped = Some(why);
                    None
                }
            },
        };
        let public = AuthorizationPointer {
            schema: AUTHORIZATION_SCHEMA.into(),
            chain: pointer.chain.clone(),
            kind: pointer.kind.clone(),
            authorization: self
                .copy_authorization(&pointer.authorization, auth_sha, &auth_bytes)
                .await?,
            renewal: match renewal {
                Some((r, sha, bytes)) => Some(self.copy_authorization(r, sha, &bytes).await?),
                None => None,
            },
            revocations: match revocations {
                Some((r, sha, bytes)) => Some(self.copy_authorization(r, sha, &bytes).await?),
                None => None,
            },
            row: match row {
                Some((r, sha, bytes)) => match self.copy_authorization(r, sha, &bytes).await {
                    Ok(copied) => Some(copied),
                    Err(why) => {
                        row_dropped = Some(why);
                        None
                    }
                },
                None => None,
            },
        };
        let bytes = serde_json::to_vec_pretty(&public).map_err(|_| {
            Refusal::Store("authorizations".into(), StoreError::Unavailable("json"))
        })?;
        put_if_changed(
            b.public,
            layout::AUTHORIZATION_CURRENT,
            bytes,
            Cache::Snapshot,
        )
        .await
        .map(|written| Copied {
            written,
            unopened,
            row_dropped,
        })
        .map_err(|e| Refusal::Store(layout::AUTHORIZATION_CURRENT.into(), e))
    }

    /// Reads and checks the coordinator's row the private pointer names
    /// (`copy_authorizations`).
    async fn checked_row(
        &self,
        r: &ObjectRef,
        auth_digest: &Digest,
    ) -> Result<(Digest, Vec<u8>), Refusal> {
        let invalid = |why: &str| Refusal::Invalid("authorizations row".into(), why.into());
        let bound = slop_ninja_admission_wire::receipts::row_object_bound();
        let _hold = self
            .budget
            .reserve(r.bytes.min(bound))
            .map_err(|_| invalid("the row exceeds the run's budget"))?;
        let (sha, bytes) = self.authorization_object_within(r, bound).await?;
        let row = Signed::<CoordinatorRow>::open(&bytes, &self.cfg.keys)
            .map_err(|e| invalid(&format!("coordinator row: {e}")))?;
        if row.chain != self.cfg.chain || row.authorization != *auth_digest {
            return Err(invalid("the row names another chain or authorization"));
        }
        Ok((sha, bytes))
    }
}

/// What one authorization copy did.
#[derive(Debug, Default)]
struct Copied {
    /// The public pointer was (re)written.
    written: bool,
    /// Entries of the revocation set that do not open under the trust roots.
    unopened: u32,
    /// The row the private pointer named was left out, and why.
    row_dropped: Option<Refusal>,
}
