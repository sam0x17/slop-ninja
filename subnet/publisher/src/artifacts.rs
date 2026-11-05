//! Publication at scoring start (the copy model: admitted A models become
//! public when scoring starts).
//!
//! Once a cohort's roster is sealed (its signed manifest is in the private
//! receipts bucket: the coordinator writes it when scoring starts), every
//! admitted entrant's artifact is copied from the private admission bucket to
//! the public bucket, entrant by entrant, retried on every run until each is
//! published or withheld, never skipped and never ended by time:
//!
//! 1. The roster comes from the signed manifest (verified under the compiled
//!    trust roots, this chain, its directory's label); each entry names the
//!    entrant's on-chain commitment, so its uploads are at the derived keys
//!    `admission/v2/c<cohort>/<commitment>/h` and `.../f/<file sha256>`
//!    (`slop_ninja_r2_presign::layout`). Those keys hold only bytes R2
//!    verified against the committed SHA-256s when they were uploaded
//!    through presigned URLs the coordinator minted for that commitment.
//! 2. Withheld for good, before anything is read: an operator or synthetic
//!    hotkey (`OPERATOR_HOTKEYS`).
//! 3. The header object: its stored SHA-256 (R2's, from `head()`, never
//!    recomputed) is the header digest; a missing checksum is refused. The
//!    header bytes (at most `MAX_ARTIFACT_HEADER_LEN`, reserved before the
//!    read) are decoded once their declared file count is within the
//!    artifact spec's table, and the inference content derived from them.
//! 4. Withheld for good: a header or content digest in `PRIVATE_DIGESTS`,
//!    or an initial reference's (references are published as reference
//!    packages, never as entrant artifacts).
//! 5. Each distinct file the header lists: the admission object's stored
//!    size and SHA-256 must be the header's (a metadata comparison).
//! 6. The copy, files first and the header last, so a public header always
//!    names files that are already public: by binding stream
//!    ([`CopyMode::Stream`], the default until a cross-bucket `CopyObject`
//!    is qualified (probe G7-x): R2 verifies the bytes against the digest
//!    as they arrive) or by `CopyObject` inside R2 ([`CopyMode::Server`],
//!    `ARTIFACT_COPY = "server"`, presigned by the internal signer Worker
//!    with the separate copy credential: metadata replaced, storage class
//!    `STANDARD`, the source etag pinned). The public copy is then checked by
//!    `head()`: its size, and its stored SHA-256 or, when R2 does not carry
//!    the checksum over to a server-side copy (design G7), its etag against
//!    the source's (`files[].check` records which).
//!
//! Nothing here hashes a byte: every integrity check compares digests R2
//! computed with the digests the commitment binds. The scores are not known
//! at scoring start, so no score-dependent rule (the public-copy gate, the
//! near-copy clusters) decides publication any more. The closed cohort's
//! publication record then reports what was published at scoring start and
//! checks it against the public result: the result's artifact digest for
//! each entrant is the header digest published for it (`publish.rs`,
//! `settle`).
//!
//! The per-cohort public record `v1/cohorts/<label>/artifacts.json`
//! ([`StartRecord`]) lists each roster entry with its state, header digest
//! and the published header and file keys.
//!
//! 7. The report to the eval Worker ([`PublicationSink`]): every decided
//!    entrant (published or withheld) and every one waiting for an absent
//!    admission object, on each run until the eval Worker takes it. Only
//!    then is the cohort done. A recorded publication lets the eval Worker
//!    delete the admitted objects once the cohort is closed; a missing
//!    object starts its audit and the miner's repair.

use std::collections::BTreeSet;
use std::future::Future;
use std::pin::Pin;

use serde::{Deserialize, Serialize};
use slop_ninja_protocol::artifact::MAX_FILE_NAME_LEN;
use slop_ninja_protocol::{
    CohortManifestV2, Digest, MAX_ARTIFACT_HEADER_LEN, RosterEntry, VerifiedHeader,
};
use slop_ninja_r2_presign::copy::{self as r2copy, CopyKind, CopySpec, parse_etag};
use slop_ninja_r2_presign::layout::{ObjectId, ObjectKind};

use crate::config::CopyMode;
use crate::index::{self, ArtifactState};
use crate::layout::{self, Cache};
use crate::publish::{
    self, Keepalive, Memo, PUBLISHER_ID, ReferenceDigests, Report, Run, WAIT_BUDGET,
    WITHHELD_INITIAL, WITHHELD_LISTED, WITHHELD_OPERATOR,
};
use crate::store::{ObjectMeta, Put, Store, StoreError};
use crate::verify::{self, ManifestCheck};

/// The scoring-start record's name in a public cohort directory.
pub const ARTIFACTS: &str = "artifacts.json";
pub const ARTIFACTS_SCHEMA: &str = "slop_ninja.public.artifacts/v1";

/// `files[].check`: the public copy's stored SHA-256 is the file's digest.
pub const CHECK_SHA256: &str = "sha256";
/// `files[].check`: R2 did not carry the checksum over to a server-side
/// copy; the copy's etag is the source's, whose SHA-256 R2 verified.
pub const CHECK_ETAG: &str = "etag";

pub const WAIT_HEADER: &str = "the header object is not in the admission bucket";
pub const WAIT_FILE: &str = "a file the header lists is not in the admission bucket";
pub const WAIT_VISIBLE: &str = "the copy is not visible in the public bucket yet";
pub const WAIT_SIGNER: &str = "ARTIFACT_COPY is server but the signer binding is not configured";
pub const REFUSED_NO_CHECKSUM: &str =
    "the header object carries no stored SHA-256 (a platform fault, never a pass)";
pub const REFUSED_HEADER_LEN: &str = "the header object's length is outside its protocol bound";
pub const REFUSED_FILE_COUNT: &str =
    "the header declares more files than the artifact spec's table";
pub const REFUSED_DECODE: &str = "the header object does not decode as an artifact header";
pub const REFUSED_DUPLICATE: &str = "the header gives one file digest two lengths";
pub const REFUSED_FILE_MISMATCH: &str =
    "a file object's stored size or SHA-256 differs from the header";
pub const REFUSED_R2: &str = "R2 refused the copy: the bytes differ from the digest";
pub const REFUSED_ETAG: &str = "the source's etag is not a single-part etag";
pub const REFUSED_COPY: &str =
    "the public object under the digest's key differs from the source (size, SHA-256 or etag)";
pub const REFUSED_SERVER_COPY: &str =
    "R2 refused the server-side copy (403, or 501 for a request it does not take)";

/// The largest file table any compiled admission spec admits.
pub const MAX_FILES: usize = slop_ninja_artifact_check::spec::ROLES.len();

/// The decoded header's held bytes, beyond its encoded bytes: the file
/// table at its largest (each entry its longest name and the fixed fields),
/// and the derived inference content (one component per file).
pub const HEADER_DECODE_BYTES: u64 = (MAX_FILES
    * (MAX_FILE_NAME_LEN + 2 * core::mem::size_of::<slop_ninja_protocol::ArtifactFile>()))
    as u64;

/// One published file.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PublishedFile {
    pub sha256: String,
    pub bytes: u64,
    /// `blobs/sha256/<sha256>`.
    pub path: String,
    /// [`CHECK_SHA256`] or [`CHECK_ETAG`].
    pub check: String,
}

/// One roster entry's publication at scoring start.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StartEntry {
    pub uid: u16,
    /// SS58 (prefix 42).
    pub hotkey: String,
    /// The on-chain commitment digest the roster names.
    pub commitment: String,
    pub state: ArtifactState,
    /// The header's SHA-256 (R2's stored checksum), once read.
    pub header: Option<String>,
    /// `artifacts/v2/<header>/header`, once published.
    pub path: Option<String>,
    pub reason: Option<String>,
    /// The files the header lists, each once, once published.
    pub files: Vec<PublishedFile>,
}

/// `v1/cohorts/<label>/artifacts.json`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StartRecord {
    pub schema: String,
    pub cohort: u64,
    pub label: String,
    pub publisher: String,
    /// `stream` or `server` ([`CopyMode`]).
    pub copy: String,
    /// Roster order.
    pub entrants: Vec<StartEntry>,
}

impl StartRecord {
    /// Every roster entry pending.
    pub fn fresh(m: &CohortManifestV2, copy: CopyMode) -> Self {
        Self {
            schema: ARTIFACTS_SCHEMA.into(),
            cohort: m.cohort,
            label: m.cohort_label.clone(),
            publisher: PUBLISHER_ID.into(),
            copy: copy.as_str().into(),
            entrants: m
                .roster
                .iter()
                .map(|r| StartEntry {
                    uid: r.identity.uid,
                    hotkey: crate::ss58::encode(&r.identity.hotkey),
                    commitment: r.commitment.hex(),
                    state: ArtifactState::Pending,
                    header: None,
                    path: None,
                    reason: None,
                    files: Vec::new(),
                })
                .collect(),
        }
    }

    /// The entry of `hotkey` (SS58).
    pub fn entry(&self, hotkey: &str) -> Option<&StartEntry> {
        self.entrants.iter().find(|e| e.hotkey == hotkey)
    }

    /// Whether every entry is decided (published or withheld).
    pub fn decided(&self) -> bool {
        self.entrants
            .iter()
            .all(|e| e.state != ArtifactState::Pending)
    }
}

/// A server-side copy: asks the signer for the presigned `CopyObject` of
/// `spec` and sends it (no body). `Ok` once R2 answered success; the caller
/// checks the copy by `head()`.
pub trait ServerCopy {
    fn copy<'a>(
        &'a self,
        spec: CopySpec,
    ) -> Pin<Box<dyn Future<Output = Result<(), StoreError>> + 'a>>;
}

/// One roster entry of the report to the eval Worker
/// (`POST /eval/admission/published`): `published` or `withheld` once
/// decided for good, `missing` while an admission object is absent.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReportEntry {
    pub commitment: String,
    pub state: String,
}

/// The eval Worker's publication report route, through its service binding
/// with the publisher's own status token. Recording a decided entrant's
/// publication ends its objects' retention in the admission bucket once its
/// cohort is closed (the eval Worker never deletes an admitted entrant's
/// objects before); reporting a missing object starts the eval Worker's
/// audit, which lists it for the miner's repair (design R4, K2). The report
/// names the cohort by number and label: the eval Worker routes it to the
/// authority that ran the cohort (its predecessor too, after a succession)
/// and answers 404 when that authority holds no such cohort, so only a
/// recorded report is ever taken.
pub trait PublicationSink {
    fn report<'a>(
        &'a self,
        cohort: u64,
        label: &'a str,
        entries: &'a [ReportEntry],
    ) -> Pin<Box<dyn Future<Output = Result<(), StoreError>> + 'a>>;
}

/// Scoring-start publication waits for the eval Worker to take the report.
pub const WAIT_REPORT: &str = "the eval Worker has not taken the publication report yet";

/// What one entrant's step decided.
enum Outcome {
    Published {
        header: Digest,
        files: Vec<PublishedFile>,
    },
    Withheld(&'static str),
    /// Not yet; retried on the next run.
    Pending(String),
    /// Not yet, and alerted: something our side or R2 should have ruled out.
    Refused(String),
}

/// The file count an encoded header record declares, read before anything
/// is decoded into owned values: the record kind and version, the contract
/// string and the sequence prefix.
fn declared_file_count(bytes: &[u8]) -> Option<u32> {
    slop_ninja_artifact_check::admission::declared_file_count(bytes)
}

/// Checks a public object `d` at a digest key against the source's length,
/// digest and etag, for copies made in `mode`. A server-side `CopyObject`
/// (metadata `REPLACE`) keeps the source's etag but never carries its stored
/// SHA-256 (qualification G7), so a copy this run just made (`fresh`) must
/// show the admitted source's etag; a stream copy is a binding `put` R2
/// verified against the digest, so it must show that SHA-256. An object a
/// run found already there (another entrant's file of the same digest, an
/// earlier run's copy) passes by either, the mode's own evidence first.
fn check_copy(
    d: &ObjectMeta,
    len: u64,
    sha: &Digest,
    src_etag: Option<&str>,
    mode: CopyMode,
    fresh: bool,
) -> Result<&'static str, Outcome> {
    let refused = || Outcome::Refused(REFUSED_COPY.into());
    if d.size != len {
        return Err(refused());
    }
    let sha_ok = match d.sha256 {
        Some(s) if s == *sha => true,
        Some(_) => return Err(refused()),
        None => false,
    };
    let etag_ok = matches!(
        (src_etag.and_then(parse_etag), d.etag.as_deref().and_then(parse_etag)),
        (Some(a), Some(b)) if a == b
    );
    let (own, other) = match mode {
        CopyMode::Server => ((etag_ok, CHECK_ETAG), (sha_ok, CHECK_SHA256)),
        CopyMode::Stream => ((sha_ok, CHECK_SHA256), (etag_ok, CHECK_ETAG)),
    };
    match (own, other) {
        ((true, check), _) => Ok(check),
        (_, (true, check)) if !fresh => Ok(check),
        _ => Err(refused()),
    }
}

impl<S: Store, K: Keepalive> Run<'_, S, K> {
    /// Publishes, at scoring start, the artifacts of every sealed in-record
    /// cohort not yet fully decided (module docs). Cohorts are independent:
    /// one that is not ready holds no other.
    pub async fn scoring_start(
        &self,
        idx: &index::CohortIndex,
        memo: &mut Memo,
        report: &mut Report,
    ) -> Result<(), String> {
        if !self.cfg.publish_artifacts {
            return Ok(());
        }
        // Inside both the public record and the artifacts boundary.
        let (Some(record), Some(artifacts)) = (self.cfg.publish_from_epoch, self.cfg.artifacts_from_epoch) else {
            return Ok(());
        };
        let from = record.max(artifacts);
        let b = &self.buckets;
        let dirs = b
            .receipts
            .list_dirs(layout::PRIVATE_COHORTS)
            .await
            .map_err(|e| e.to_string())?;
        let labels: Vec<String> = dirs
            .iter()
            .map(|d| {
                d.trim_start_matches(layout::PRIVATE_COHORTS)
                    .trim_end_matches('/')
                    .to_owned()
            })
            .collect();
        let live: BTreeSet<&str> = labels.iter().map(String::as_str).collect();
        memo.artifacts_done.retain(|l| live.contains(l.as_str()));
        for label in labels {
            if !layout::safe_label(&label)
                || memo.artifacts_done.contains(&label)
                || memo.outside_record.contains_key(&label)
                || memo.unverifiable.contains_key(&label)
                || self
                    .cfg
                    .record_label_prefix
                    .as_ref()
                    .is_some_and(|p| !label.starts_with(p))
            {
                continue;
            }
            let private = layout::private_cohort_dir(&label);
            match b
                .receipts
                .head(&layout::join(&private, verify::MANIFEST))
                .await
            {
                Ok(Some(_)) => {}
                // Not sealed yet: scoring has not started.
                Ok(None) => continue,
                Err(e) => {
                    report
                        .pending
                        .push((format!("{label}/{ARTIFACTS}"), e.to_string()));
                    continue;
                }
            }
            if !self.may_continue().await {
                report
                    .pending
                    .push((format!("{label}/{ARTIFACTS}"), WAIT_BUDGET.into()));
                break;
            }
            let m = match verify::open_manifest(
                b.receipts,
                &private,
                &label,
                &self.opts(),
                self.budget,
            )
            .await
            {
                Ok(ManifestCheck::Opened(m)) => *m,
                // Never a reward-v1 cohort of this chain: nothing to publish.
                Ok(
                    ManifestCheck::OtherKind
                    | ManifestCheck::OtherChain
                    | ManifestCheck::OtherLabel,
                ) => {
                    memo.artifacts_done.insert(label);
                    continue;
                }
                // The closed-cohort listing alerts it; a new build may open it.
                Ok(ManifestCheck::Unverifiable(_)) => continue,
                Err(e) => {
                    report
                        .pending
                        .push((format!("{label}/{ARTIFACTS}"), publish::public_reason(&e)));
                    continue;
                }
            };
            // Before the record, or with nothing admitted (a probe build):
            // nothing to publish, and no record is written.
            if m.opened_epoch < from || m.roster.is_empty() {
                memo.artifacts_done.insert(label);
                continue;
            }
            if let Some(r) = index::initial_reference(&m.references) {
                memo.references.insert(ReferenceDigests::of(&r));
            }
            let references = self.withheld_references(memo, idx, None);
            let (decided, entries) = self.publish_at_start(&m, &references, report).await?;
            // Reported on every run until taken: a decided cohort is done
            // only once the eval Worker recorded it.
            let reported = self.report_publication(m.cohort, &label, &entries, report).await;
            if decided && reported {
                memo.artifacts_done.insert(label);
            }
        }
        Ok(())
    }

    /// Reports `entries` to the eval Worker ([`PublicationSink`]); `true`
    /// once taken (or when there is nothing to report).
    async fn report_publication(
        &self,
        cohort: u64,
        label: &str,
        entries: &[ReportEntry],
        report: &mut Report,
    ) -> bool {
        if entries.is_empty() {
            return true;
        }
        let taken = match self.buckets.publication {
            Some(sink) => sink.report(cohort, label, entries).await.is_ok(),
            None => false,
        };
        if !taken {
            report
                .pending
                .push((format!("{label}/{ARTIFACTS}"), WAIT_REPORT.into()));
        }
        taken
    }

    /// One cohort's step: settles each pending roster entry and writes the
    /// record when it changed. Returns whether every entry is decided, and
    /// the entries to report to the eval Worker (each decided one, and each
    /// waiting for an admission object that is absent).
    async fn publish_at_start(
        &self,
        m: &CohortManifestV2,
        references: &BTreeSet<ReferenceDigests>,
        report: &mut Report,
    ) -> Result<(bool, Vec<ReportEntry>), String> {
        let label = &m.cohort_label;
        let key = layout::artifacts_record(label);
        let p = self.buckets.public;
        // An unreadable stored record (an earlier build's shape) is rebuilt:
        // every object it published is found again at its digest key.
        let stored = publish::get_json::<S, StartRecord>(p, &key, self.budget)
            .await
            .unwrap_or(None);
        let mut rec = StartRecord::fresh(m, self.cfg.artifact_copy);
        if let Some(old) = stored
            .as_ref()
            .filter(|o| o.cohort == m.cohort && o.label == *label)
        {
            for e in &mut rec.entrants {
                if let Some(o) = old.entrants.iter().find(|o| {
                    o.commitment == e.commitment
                        && o.hotkey == e.hotkey
                        && o.state != ArtifactState::Pending
                }) {
                    *e = o.clone();
                }
            }
        }
        for (r, e) in m.roster.iter().zip(rec.entrants.iter_mut()) {
            if e.state != ArtifactState::Pending {
                continue;
            }
            let subject = format!("{label}/artifact-{}", &e.commitment[..16]);
            if !self.may_continue().await {
                e.reason = Some(WAIT_BUDGET.into());
                report.pending.push((subject, WAIT_BUDGET.into()));
                break;
            }
            match self.publish_entrant(m.cohort, r, references).await {
                Outcome::Published { header, files } => {
                    e.state = ArtifactState::Published;
                    e.header = Some(header.hex());
                    e.path = Some(layout::header(&header));
                    e.files = files;
                    e.reason = None;
                    report.artifacts_copied += 1;
                }
                Outcome::Withheld(why) => {
                    e.state = ArtifactState::Withheld;
                    e.reason = Some(why.into());
                }
                Outcome::Pending(why) => {
                    report.pending.push((subject, why.clone()));
                    e.reason = Some(why);
                }
                Outcome::Refused(why) => {
                    report.refused.push((subject, why.clone()));
                    e.reason = Some(why);
                }
            }
        }
        if stored.as_ref() != Some(&rec) {
            publish::put_json(p, &key, &rec, Cache::Index)
                .await
                .map_err(|e| format!("{key}: {e}"))?;
        }
        let entries = rec
            .entrants
            .iter()
            .filter_map(|e| {
                let state = match e.state {
                    ArtifactState::Published => "published",
                    ArtifactState::Withheld => "withheld",
                    ArtifactState::Pending
                        if matches!(e.reason.as_deref(), Some(WAIT_FILE | WAIT_HEADER)) =>
                    {
                        "missing"
                    }
                    ArtifactState::Pending => return None,
                };
                Some(ReportEntry {
                    commitment: e.commitment.clone(),
                    state: state.into(),
                })
            })
            .collect();
        Ok((rec.decided(), entries))
    }

    async fn publish_entrant(
        &self,
        cohort: u64,
        r: &RosterEntry,
        references: &BTreeSet<ReferenceDigests>,
    ) -> Outcome {
        if self.cfg.operator_hotkeys.contains(&r.identity.hotkey) {
            return Outcome::Withheld(WITHHELD_OPERATOR);
        }
        let a = self.buckets.admission;
        let commitment = r.commitment.0;
        let hkey = ObjectId {
            cohort,
            commitment,
            kind: ObjectKind::Header,
        }
        .key();
        let meta = match a.head(hkey.as_str()).await {
            Ok(Some(m)) => m,
            Ok(None) => return Outcome::Pending(WAIT_HEADER.into()),
            Err(e) => return Outcome::Pending(e.to_string()),
        };
        let Some(header_sha) = meta.sha256 else {
            return Outcome::Refused(REFUSED_NO_CHECKSUM.into());
        };
        if meta.size == 0 || meta.size > u64::from(MAX_ARTIFACT_HEADER_LEN) {
            return Outcome::Refused(REFUSED_HEADER_LEN.into());
        }
        let header = {
            let Ok(_hold) = self.budget.reserve(meta.size + HEADER_DECODE_BYTES) else {
                return Outcome::Pending(format!("{WAIT_BUDGET}: the header's read and decode"));
            };
            let bytes = match a.get(hkey.as_str(), meta.size).await {
                Ok(Some(b)) => b,
                Ok(None) => return Outcome::Pending(WAIT_HEADER.into()),
                Err(StoreError::TooLarge { .. }) => {
                    return Outcome::Refused(REFUSED_HEADER_LEN.into());
                }
                Err(e) => return Outcome::Pending(e.to_string()),
            };
            if !declared_file_count(&bytes).is_some_and(|n| n as usize <= MAX_FILES) {
                return Outcome::Refused(REFUSED_FILE_COUNT.into());
            }
            match VerifiedHeader::from_stored(&bytes, header_sha) {
                Ok(h) => h,
                Err(_) => return Outcome::Refused(REFUSED_DECODE.into()),
            }
        };
        if self.cfg.private_digests.contains(&header.content)
            || self.cfg.private_digests.contains(&header_sha)
        {
            return Outcome::Withheld(WITHHELD_LISTED);
        }
        let (content, artifact) = (header.content.hex(), header_sha.hex());
        if references
            .iter()
            .any(|x| x.content == content || x.artifact == artifact)
        {
            return Outcome::Withheld(WITHHELD_INITIAL);
        }
        // Each distinct file once: its stored size and checksum against the
        // header (no byte read).
        let mut files: Vec<(Digest, u64, Option<String>)> =
            Vec::with_capacity(header.header.files.len());
        for f in &header.header.files {
            if let Some(seen) = files.iter().find(|x| x.0 == f.sha256) {
                if seen.1 != f.len {
                    return Outcome::Refused(REFUSED_DUPLICATE.into());
                }
                continue;
            }
            let fkey = ObjectId {
                cohort,
                commitment,
                kind: ObjectKind::File(f.sha256.0),
            }
            .key();
            match a.head(fkey.as_str()).await {
                Ok(Some(fm)) if fm.size == f.len && fm.sha256 == Some(f.sha256) => {
                    files.push((f.sha256, f.len, fm.etag))
                }
                Ok(Some(_)) => return Outcome::Refused(REFUSED_FILE_MISMATCH.into()),
                Ok(None) => return Outcome::Pending(WAIT_FILE.into()),
                Err(e) => return Outcome::Pending(e.to_string()),
            }
        }
        // Files first, the header last: a public header names public files.
        let mut published = Vec::with_capacity(files.len());
        for (sha, len, etag) in files {
            let spec = CopySpec {
                cohort,
                commitment,
                kind: CopyKind::File,
                sha256: sha.0,
                etag: [0; 16],
            };
            match self.copy_object(spec, len, etag.as_deref()).await {
                Ok(check) => published.push(PublishedFile {
                    sha256: sha.hex(),
                    bytes: len,
                    path: layout::blob(&sha),
                    check: check.into(),
                }),
                Err(o) => return o,
            }
        }
        let spec = CopySpec {
            cohort,
            commitment,
            kind: CopyKind::Header,
            sha256: header_sha.0,
            etag: [0; 16],
        };
        match self
            .copy_object(spec, meta.size, meta.etag.as_deref())
            .await
        {
            Ok(_) => Outcome::Published {
                header: header_sha,
                files: published,
            },
            Err(o) => o,
        }
    }

    /// Copies one admission object to its public digest key unless the key
    /// already holds it, then checks the public object. `spec.etag` is
    /// filled here from `src_etag`.
    async fn copy_object(
        &self,
        mut spec: CopySpec,
        len: u64,
        src_etag: Option<&str>,
    ) -> Result<&'static str, Outcome> {
        let p = self.buckets.public;
        let digest = Digest(spec.sha256);
        let dst = spec.destination_key();
        let pending = |e: StoreError| Outcome::Pending(e.to_string());
        if let Some(d) = p.head(dst.as_str()).await.map_err(pending)? {
            return check_copy(&d, len, &digest, src_etag, self.cfg.artifact_copy, false);
        }
        if !self.may_continue().await {
            return Err(Outcome::Pending(WAIT_BUDGET.into()));
        }
        match self.cfg.artifact_copy {
            CopyMode::Stream => {
                let put = Put {
                    content_type: r2copy::CONTENT_TYPE,
                    cache: Cache::Immutable,
                    sha256: Some(&digest),
                };
                let src = spec.source_key();
                match p
                    .copy_artifact_from(
                        self.buckets.admission,
                        src.as_str(),
                        dst.as_str(),
                        len,
                        &put,
                    )
                    .await
                {
                    Ok(true) => {}
                    Ok(false) => return Err(Outcome::Pending(WAIT_FILE.into())),
                    Err(StoreError::Integrity) => return Err(Outcome::Refused(REFUSED_R2.into())),
                    Err(e) => return Err(pending(e)),
                }
            }
            CopyMode::Server => {
                let Some(server) = self.buckets.server_copy else {
                    return Err(Outcome::Pending(WAIT_SIGNER.into()));
                };
                let Some(etag) = src_etag.and_then(parse_etag) else {
                    return Err(Outcome::Refused(REFUSED_ETAG.into()));
                };
                spec.etag = etag;
                match server.copy(spec).await {
                    Ok(()) => {}
                    Err(StoreError::Refused(status)) => {
                        return Err(Outcome::Refused(format!("{REFUSED_SERVER_COPY} ({status})")));
                    }
                    Err(e) => return Err(pending(e)),
                }
            }
        }
        match p.head(dst.as_str()).await.map_err(pending)? {
            Some(d) => check_copy(&d, len, &digest, src_etag, self.cfg.artifact_copy, true),
            None => Err(Outcome::Pending(WAIT_VISIBLE.into())),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn meta(size: u64, sha: Option<Digest>, etag: Option<&str>) -> ObjectMeta {
        ObjectMeta {
            size,
            sha256: sha,
            etag: etag.map(str::to_owned),
        }
    }

    #[test]
    fn a_copy_is_checked_by_its_digest_or_else_the_sources_etag() {
        use CopyMode::{Server, Stream};
        let sha = Digest([7; 32]);
        let etag = "5d41402abc4b2a76b9719d911017c592";
        let other = "00000000000000000000000000000000";
        // An object found already there: either evidence ties it, the
        // mode's own first.
        for mode in [Server, Stream] {
            assert_eq!(
                check_copy(&meta(5, Some(sha), None), 5, &sha, None, mode, false).ok(),
                Some(CHECK_SHA256)
            );
            assert_eq!(
                check_copy(&meta(5, None, Some(etag)), 5, &sha, Some(etag), mode, false).ok(),
                Some(CHECK_ETAG)
            );
            // Quoted or not, the same etag.
            let quoted = format!("\"{etag}\"");
            assert_eq!(
                check_copy(&meta(5, None, Some(&quoted)), 5, &sha, Some(etag), mode, false).ok(),
                Some(CHECK_ETAG)
            );
            for bad in [
                meta(6, Some(sha), None),
                meta(5, Some(Digest([8; 32])), Some(etag)),
                meta(5, None, Some(other)),
                meta(5, None, None),
            ] {
                assert!(check_copy(&bad, 5, &sha, Some(etag), mode, false).is_err());
            }
            // Without a source etag, only the digest can tie the copy.
            assert!(check_copy(&meta(5, None, Some(etag)), 5, &sha, None, mode, false).is_err());
        }
        let both = meta(5, Some(sha), Some(etag));
        assert_eq!(check_copy(&both, 5, &sha, Some(etag), Server, false).ok(), Some(CHECK_ETAG));
        assert_eq!(check_copy(&both, 5, &sha, Some(etag), Stream, false).ok(), Some(CHECK_SHA256));
        // A copy just made must show its own mode's evidence: CopyObject
        // keeps the etag but not the checksum (G7), a stream put the checksum.
        assert_eq!(
            check_copy(&meta(5, None, Some(etag)), 5, &sha, Some(etag), Server, true).ok(),
            Some(CHECK_ETAG)
        );
        for bad in [meta(5, Some(sha), None), meta(5, None, Some(other)), meta(4, None, Some(etag))] {
            assert!(check_copy(&bad, 5, &sha, Some(etag), Server, true).is_err());
        }
        assert_eq!(check_copy(&both, 5, &sha, Some(etag), Stream, true).ok(), Some(CHECK_SHA256));
        assert!(check_copy(&meta(5, None, Some(etag)), 5, &sha, Some(etag), Stream, true).is_err());
    }

    #[test]
    fn bounds_come_from_the_protocol_and_the_spec() {
        assert_eq!(MAX_FILES, 6);
        const { assert!(HEADER_DECODE_BYTES < 64 * 1024) };
    }
}
