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
//!    must order like their opening epochs;
//! 4. every public object is copied byte for byte, the destination checking
//!    each against the digest verified in step 3, and the copies are verified
//!    again from the public bucket;
//! 5. artifacts are published default-deny ([`static_withhold`], then the
//!    entrant's admission record, [`EntrantFacts::admitted_by`]);
//! 6. the publication record and the index entry are written, then, for the
//!    newest cohort, the leaderboard, the page and the reference pointers.
//!
//! A refused cohort is remembered with the publisher build that refused it
//! and is not read again until a new build (a new version or new trust
//! roots); its alert stays raised. Artifacts not copied in one run stay
//! pending and are retried by later runs. The current validator
//! authorization and its renewal are copied once their cohort is listed
//! ([`Run::authorizations`]).
//!
//! Detailed receipts follow the N+2 rule on the public host: once two later
//! cohorts are published and N's target is superseded, cohort N's receipt
//! index, chunk map and bundles are deleted ([`Run::retention`]). The signed
//! manifest, package (with the receipt root and count), target and closure
//! stay. Nothing else is ever deleted, and nothing is written to the private
//! buckets.

use std::cell::Cell;
use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use slop_ninja_admission_wire::json::Chain;
use slop_ninja_protocol::submission::SubmissionUpload;
use slop_ninja_protocol::{
    AccountId, CalculationTargetV2, Digest, EntrantScore, LaunchAuthorization, Nonce, Record,
    ReferenceModel, RewardIntentAuthorization, RewardIntentRenewal, Signed,
};

use crate::arena::{self, Budget};
use crate::config::{Config, TRUST_FILE};
use crate::index::{
    self, ArtifactEntry, ArtifactState, CohortIndex, Publication, ReferencePointer,
};
use crate::layout::{self, Cache};
use crate::leaderboard::{self, LeaderboardDoc};
use crate::store::{Put, Store, StoreError};
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
    pub public: &'a S,
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
}

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
    /// Closed cohorts outside the record seen this run.
    pub outside: u32,
    pub receipts_deleted: u32,
    pub publication_disabled: bool,
    pub index_written: bool,
    pub authorization_written: bool,
    /// A new public record started this run; the previous one was moved
    /// under `v1/records/<its first label>/`.
    pub record_restarted: Option<String>,
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
pub const WITHHELD_CLUSTER: &str = "joined the near-copy cluster of a withheld artifact";
pub const WITHHELD_NOT_ADMITTED: &str = "no outside admission record: not uploaded through the admission route under the submission terms";
pub const WITHHELD_ADMISSION: &str = "the admission record does not match the entrant";
pub const WAIT_LAYOUT: &str = "the admission object layout is not configured";
pub const WAIT_SOURCE: &str = "the artifact object is not in the eval bucket";
pub const WAIT_BUDGET: &str = "the run reached its wall budget";
pub const WAIT_STORE: &str = "the eval bucket did not answer";

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
/// no predecessor. A later cohort waits until such a cohort is listed.
pub fn record_start(idx: &CohortIndex, v: &VerifiedCohort) -> Result<(), String> {
    let refs = &v.manifest.references;
    if idx.cohorts.is_empty() && (refs.index != 0 || refs.predecessor.is_some()) {
        return Err(format!(
            "waiting for the record's first cohort: this one's reference has index {}",
            refs.index
        ));
    }
    Ok(())
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

/// The detailed receipts of a cohort: its receipt index, chunk map and
/// bundles.
pub fn is_detailed_receipt(rel: &str) -> bool {
    rel == verify::RECEIPT_INDEX || rel == verify::CHUNK_MAP || rel.starts_with("receipts/")
}

/// The cache rule of one copied cohort object: detailed receipts are
/// deleted after N+2, so caches may keep them one tempo at most; every other
/// signed object never changes.
pub fn object_cache(rel: &str, cfg: &Config) -> Cache {
    if is_detailed_receipt(rel) {
        Cache::Bounded(cfg.tempo_blocks * cfg.block_secs)
    } else {
        Cache::Immutable
    }
}

fn put_meta(key: &str, cache: Cache) -> Put<'_> {
    Put {
        content_type: layout::content_type(key),
        cache,
        sha256: None,
    }
}

async fn put_json<S: Store, T: Serialize>(
    store: &S,
    key: &str,
    value: &T,
    cache: Cache,
) -> Result<(), StoreError> {
    let bytes = serde_json::to_vec_pretty(value).map_err(|_| StoreError::Unavailable("json"))?;
    store.put(key, bytes, &put_meta(key, cache)).await
}

async fn get_json<S: Store, T: for<'d> Deserialize<'d>>(
    store: &S,
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

/// Writes `bytes` at `key` unless the store already holds them (by SHA-256).
async fn put_if_changed<S: Store>(
    store: &S,
    key: &str,
    bytes: Vec<u8>,
    cache: Cache,
) -> Result<bool, StoreError> {
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

/// Copies one verified object between buckets unless the destination
/// already holds it; the destination checks the digest.
async fn copy_checked<S: Store>(
    from: &S,
    to: &S,
    src: &str,
    dst: &str,
    len: u64,
    digest: &Digest,
    cache: Cache,
) -> Result<bool, StoreError> {
    if let Some(meta) = to.head(dst).await?
        && meta.size == len
        && meta.sha256 == Some(*digest)
    {
        return Ok(true);
    }
    let put = Put {
        content_type: layout::content_type(dst),
        cache,
        sha256: Some(digest),
    };
    to.copy_from(from, src, dst, len, &put).await
}

/// Copies one verified cohort object.
async fn copy_object<S: Store>(
    b: &Buckets<'_, S>,
    cfg: &Config,
    label: &str,
    obj: &verify::PublicObject,
) -> Result<(), Refusal> {
    let dst = layout::join(&layout::cohort_dir(label), &obj.rel);
    let digest = obj
        .digest()
        .ok_or_else(|| Refusal::Invalid(obj.rel.clone(), "digest".into()))?;
    let src = layout::join(&layout::private_cohort_dir(label), &obj.rel);
    match copy_checked(
        b.receipts,
        b.public,
        &src,
        &dst,
        obj.bytes,
        &digest,
        object_cache(&obj.rel, cfg),
    )
    .await
    {
        Ok(true) => Ok(()),
        Ok(false) => Err(Refusal::Missing(obj.rel.clone())),
        Err(StoreError::Integrity) => Err(Refusal::Invalid(
            obj.rel.clone(),
            "changed after verification".into(),
        )),
        Err(e) => Err(Refusal::Store(obj.rel.clone(), e)),
    }
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
    match b.public.copy_from(b.eval, src, &dst, meta.size, &put).await {
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
/// authorization that lets validators commit now and its renewal, each a
/// content-addressed object.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AuthorizationPointer {
    pub schema: String,
    pub chain: Chain,
    /// `launch` or `cohort`.
    pub kind: String,
    pub authorization: ObjectRef,
    #[serde(default)]
    pub renewal: Option<ObjectRef>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ObjectRef {
    pub path: String,
    pub sha256: String,
    pub bytes: u64,
}

pub const AUTHORIZATION_SCHEMA: &str = "slop_ninja.public.authorization/v1";

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
        Options {
            keys: &self.cfg.keys,
            chain: &self.cfg.chain,
            receipts: self.cfg.publish_receipts,
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
            match get_json::<S, CohortIndex>(b.public, layout::COHORT_INDEX, self.budget).await? {
                Some(i) if i.schema == index::INDEX_SCHEMA => i,
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
        if let Some(prefix) = &self.cfg.record_label_prefix {
            memo.unverifiable
                .retain(|label, _| label.starts_with(prefix));
            memo.outside_record
                .retain(|label, _| label.starts_with(prefix));
        }
        if let Some(from) = self.cfg.publish_from_epoch {
            memo.outside_record
                .retain(|_, o| o.opened_epoch.is_none_or(|e| e < from));
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
        self.authorizations(&idx, report).await;
        self.retention(&mut idx, report).await?;
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
    /// (`previous_records`: its prefix, never adopted again, its initial
    /// references, still withheld as entrant artifacts, and its cohorts whose
    /// detailed receipts are still published, which `retention` deletes under
    /// the N+2 rule against the new record). An archive object is written
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
                    p.put(
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
            receipts_pending: old
                .cohorts
                .iter()
                .filter(|c| c.receipts_retained)
                .map(|c| index::PendingReceipts {
                    label: c.label.clone(),
                    cohort: c.cohort,
                    target: c.target.clone(),
                })
                .collect(),
        });
        put_json(p, layout::COHORT_INDEX, &idx, Cache::Index)
            .await
            .map_err(|e| e.to_string())?;
        self.write_site(None).await.map_err(|e| e.to_string())?;
        report.index_written = true;
        report.record_restarted = Some(first);
        Ok(idx)
    }

    fn refuse(&self, memo: &mut Memo, report: &mut Report, label: String, reason: String) {
        memo.refused.insert(
            label.clone(),
            Remembered {
                build: build_id(),
                reason: reason.clone(),
            },
        );
        report.refused.push((label, reason));
    }

    /// Places every closed cohort not yet listed inside or outside the
    /// record, then verifies and publishes the in-record ones in chain order,
    /// at most one to completion per run.
    async fn new_cohorts(
        &self,
        from: u64,
        idx: &mut CohortIndex,
        memo: &mut Memo,
        report: &mut Report,
    ) -> Result<(), String> {
        let b = &self.buckets;
        let dirs = b
            .receipts
            .list_dirs(layout::PRIVATE_COHORTS)
            .await
            .map_err(|e| e.to_string())?;
        let mut candidates: Vec<(u64, String)> = Vec::new();
        for dir in dirs {
            let label = dir
                .trim_start_matches(layout::PRIVATE_COHORTS)
                .trim_end_matches('/')
                .to_owned();
            if !layout::safe_label(&label) || idx.contains(&label) {
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
                Ok(None) => continue,
                Err(e) => {
                    report.pending.push((label, e.to_string()));
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
                            label,
                            Outside {
                                opened_epoch: Some(m.opened_epoch),
                                reason: "opened before PUBLISH_FROM_EPOCH".into(),
                            },
                        );
                        report.outside += 1;
                    } else {
                        candidates.push((m.opened_epoch, label));
                    }
                }
                Ok(ManifestCheck::Unverifiable(why)) => {
                    memo.unverifiable.insert(
                        label.clone(),
                        Remembered {
                            build: build_id(),
                            reason: format!("manifest.signed: {why}"),
                        },
                    );
                    report.unverifiable.push(label);
                }
                Ok(check @ (ManifestCheck::OtherChain | ManifestCheck::OtherKind)) => {
                    let reason = match check {
                        ManifestCheck::OtherChain => "another chain or subnet",
                        _ => "not a reward-v1 cohort manifest",
                    };
                    memo.outside_record.insert(
                        label,
                        Outside {
                            opened_epoch: None,
                            reason: reason.into(),
                        },
                    );
                    report.outside += 1;
                }
                Ok(ManifestCheck::OtherLabel) => self.refuse(
                    memo,
                    report,
                    label,
                    "manifest.signed: label differs from its directory".into(),
                ),
                Err(r) if r.retryable() => report.pending.push((label, r.to_string())),
                Err(r) => self.refuse(memo, report, label, r.to_string()),
            }
        }
        candidates.sort();
        for (_, label) in candidates {
            if !self.may_continue().await {
                break;
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
                Err(r) if r.retryable() => {
                    report.pending.push((label, r.to_string()));
                    continue;
                }
                Err(r) => {
                    self.refuse(memo, report, label, r.to_string());
                    break;
                }
            };
            if let Err(why) = record_start(idx, &v) {
                report.pending.push((label, why));
                continue;
            }
            if let Err(why) = numbering(idx, &v) {
                self.refuse(memo, report, label, why);
                break;
            }
            match self.publish_one(&v, idx, memo, report).await {
                Ok(()) => {
                    report.published.push(label);
                    break;
                }
                Err(r) if r.retryable() => report.pending.push((label, r.to_string())),
                Err(r) => {
                    self.refuse(memo, report, label, r.to_string());
                    break;
                }
            }
        }
        Ok(())
    }

    /// Deletes the detailed receipts of every published cohort N once two
    /// later cohorts are published (N+1 and N+2 completed) and N's target,
    /// if it produced one, is superseded by a later cohort's; records it.
    /// While N's target is the active target its receipts stay, since new
    /// commitments to it need them. An earlier record's cohorts that still
    /// had their receipts when it was moved aside (`previous_records`) are
    /// held to the two-later-cohorts part of the rule against the live
    /// record's cohorts: the restart superseded their targets (the
    /// coordinator's `restart_target` and the validators' `reset-target` set
    /// the held target aside, so no new commitment to an earlier record's
    /// target is made), so a record restart leaves no detailed receipt
    /// published indefinitely, even while the new record produces no target.
    pub async fn retention(
        &self,
        idx: &mut CohortIndex,
        report: &mut Report,
    ) -> Result<(), String> {
        // Cohort N's detailed receipts once N+1 and N+2 are published and a
        // later target supersedes N's (none: nothing to supersede): the live
        // record's own cohorts, and an earlier record's that were still
        // published when it was moved aside (cohort numbers continue across
        // authorities, so the live record's cohorts are their N+1 and N+2).
        let due_after = |cohort: u64, target: &Option<String>| {
            let later: Vec<&index::CohortEntry> =
                idx.cohorts.iter().filter(|o| o.cohort > cohort).collect();
            let superseded = target.is_none() || later.iter().any(|o| o.target.is_some());
            let mut numbers: Vec<u64> = later.iter().map(|o| o.cohort).collect();
            numbers.sort_unstable();
            numbers.get(1).filter(|_| superseded).copied()
        };
        let live: Vec<(String, u64)> = idx
            .cohorts
            .iter()
            .filter(|c| c.receipts_retained)
            .filter_map(|c| due_after(c.cohort, &c.target).map(|n| (c.label.clone(), n)))
            .collect();
        let earlier: Vec<(String, u64)> = idx
            .previous_records
            .iter()
            .flat_map(|r| r.receipts_pending.iter())
            .filter_map(|c| due_after(c.cohort, &None).map(|n| (c.label.clone(), n)))
            .collect();
        for (label, after) in live {
            if !self.delete_receipts(&label, after, report).await? {
                continue;
            }
            if let Some(entry) = idx.cohorts.iter_mut().find(|c| c.label == label) {
                entry.receipts_retained = false;
            }
            self.receipts_deleted(idx, report).await?;
        }
        for (label, after) in earlier {
            if !self.delete_receipts(&label, after, report).await? {
                continue;
            }
            for r in &mut idx.previous_records {
                r.receipts_pending.retain(|c| c.label != label);
            }
            self.receipts_deleted(idx, report).await?;
        }
        Ok(())
    }

    /// Deletes cohort `label`'s detailed receipts from the public host and
    /// records `after` in its publication record. `false` (reported) when
    /// that record is missing: nothing is deleted.
    async fn delete_receipts(
        &self,
        label: &str,
        after: u64,
        report: &mut Report,
    ) -> Result<bool, String> {
        let b = &self.buckets;
        let Some(mut publication) =
            get_json::<S, Publication>(b.public, &layout::publication(label), self.budget).await?
        else {
            report.refused.push((
                label.to_owned(),
                "publication record missing; receipts not deleted".into(),
            ));
            return Ok(false);
        };
        let dir = layout::cohort_dir(label);
        for o in publication
            .objects
            .iter()
            .filter(|o| is_detailed_receipt(&o.rel))
        {
            b.public
                .delete(&layout::join(&dir, &o.rel))
                .await
                .map_err(|e| e.to_string())?;
        }
        publication.receipts_deleted_after = Some(after);
        put_json(
            b.public,
            &layout::publication(label),
            &publication,
            Cache::Index,
        )
        .await
        .map_err(|e| e.to_string())?;
        Ok(true)
    }

    async fn receipts_deleted(&self, idx: &CohortIndex, report: &mut Report) -> Result<(), String> {
        put_json(self.buckets.public, layout::COHORT_INDEX, idx, Cache::Index)
            .await
            .map_err(|e| e.to_string())?;
        report.index_written = true;
        report.receipts_deleted += 1;
        Ok(())
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
        let objects = v.public_objects();
        for obj in &objects {
            copy_object(b, self.cfg, label, obj).await?;
        }
        let copied = verify::verify_cohort(
            b.public,
            &layout::cohort_dir(label),
            label,
            &self.opts(),
            self.budget,
        )
        .await
        .map_err(|r| match r {
            Refusal::Missing(rel) => Refusal::Invalid(rel, "missing after the copy".into()),
            other => other,
        })?;
        if copied.public_objects() != objects {
            return Err(Refusal::Invalid(
                label.clone(),
                "copies differ from the verified objects".into(),
            ));
        }
        // Artifacts, default-deny: the static checks, then the cluster of a
        // withheld artifact, then the admission record and the copy.
        let references = self.withheld_references(memo, idx, Some(v));
        let entrants = &v.package.entrants;
        let facts: Vec<EntrantFacts> = entrants.iter().map(|e| EntrantFacts::of(e, v)).collect();
        let mut withheld: Vec<Option<&'static str>> = entrants
            .iter()
            .zip(&facts)
            .map(|(e, f)| static_withhold(f, Some(e), self.cfg, &references))
            .collect();
        for (i, e) in entrants.iter().enumerate() {
            if withheld[i].is_some() {
                continue;
            }
            let head = e
                .cluster_head
                .filter(|h| *h != e.identity.hotkey)
                .and_then(|h| entrants.iter().position(|x| x.identity.hotkey == h));
            if head.is_some_and(|j| withheld[j].is_some()) {
                withheld[i] = Some(WITHHELD_CLUSTER);
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
            objects,
            receipts_verified: v.receipts,
            components_withheld: match &v.components {
                verify::Components::Withheld(why) => Some(why.clone()),
                _ => None,
            },
            artifacts: artifacts.clone(),
            receipts_deleted_after: None,
        };
        let store_err = |e| Refusal::Store(label.clone(), e);
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
        idx.upsert(index::entry(v, &artifacts), &self.cfg.chain);
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
                cohort: v,
                artifacts: &artifacts,
                active_target: &idx.active_target,
                current_reference: current,
                previous_reference: previous,
                chain: Chain::from_binding(&self.cfg.chain),
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
    /// the static checks repeated (the configuration may list more since),
    /// and refreshes the records that name them.
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
            let mut changed = false;
            for a in publication
                .artifacts
                .iter_mut()
                .filter(|a| a.state == ArtifactState::Pending)
            {
                let before = a.clone();
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
                        None => self.admit_and_copy(&f, &label, a, report).await,
                    },
                }
                changed |= *a != before;
            }
            if !changed {
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
                .find(|a| a.hotkey == row.hotkey)
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

    /// Copies the current validator authorization and its renewal, then
    /// points `v1/authorizations/current.json` at them; reports problems.
    pub async fn authorizations(&self, idx: &CohortIndex, report: &mut Report) {
        match self.copy_authorizations(idx).await {
            Ok(written) => report.authorization_written = written,
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
        let what = "authorization object";
        let sha = digest_hex(&r.sha256)
            .filter(|d| r.path == layout::private_authorization(d))
            .ok_or_else(|| Refusal::Invalid(what.into(), "not a content-addressed key".into()))?;
        let bytes = match self
            .buckets
            .receipts
            .get(&r.path, arena::AUTHORIZATION_OBJECT_BOUND)
            .await
        {
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

    /// Copies one verified authorization object to its public key.
    async fn copy_authorization(&self, r: &ObjectRef, sha: Digest) -> Result<ObjectRef, Refusal> {
        let b = &self.buckets;
        let dst = layout::authorization(&sha);
        match copy_checked(
            b.receipts,
            b.public,
            &r.path,
            &dst,
            r.bytes,
            &sha,
            Cache::Immutable,
        )
        .await
        {
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

    async fn copy_authorizations(&self, idx: &CohortIndex) -> Result<bool, Refusal> {
        let b = &self.buckets;
        let pointer: AuthorizationPointer = match b
            .receipts
            .get(
                layout::PRIVATE_AUTHORIZATION_POINTER,
                arena::AUTHORIZATION_POINTER_BOUND,
            )
            .await
        {
            Ok(None) => return Ok(false),
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
                if a.chain != self.cfg.chain {
                    return Err(invalid("the authorization names another chain or subnet"));
                }
                let listed = idx.cohorts.iter().any(|c| {
                    c.cohort == a.cohort
                        && c.manifest == a.cohort_manifest.hex()
                        && c.package == a.score_package.hex()
                });
                if !listed {
                    // Published once its cohort is listed.
                    return Err(Refusal::Missing(format!(
                        "the authorization's cohort {} is not listed yet",
                        a.cohort
                    )));
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
                Some((r, sha))
            }
        };
        let public = AuthorizationPointer {
            schema: AUTHORIZATION_SCHEMA.into(),
            chain: pointer.chain.clone(),
            kind: pointer.kind.clone(),
            authorization: self
                .copy_authorization(&pointer.authorization, auth_sha)
                .await?,
            renewal: match renewal {
                Some((r, sha)) => Some(self.copy_authorization(r, sha).await?),
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
        .map_err(|e| Refusal::Store(layout::AUTHORIZATION_CURRENT.into(), e))
    }
}
