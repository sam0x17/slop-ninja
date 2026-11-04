//! The public cohort index (`v1/cohorts/index.json`), each cohort's
//! publication record (`v1/cohorts/<label>/publication.json`) and the
//! reference pointers (`v1/reference/{current,previous}.json`).
//!
//! The index is the publisher's durable state: a cohort is published exactly
//! when it has an entry, and an entry is written only after the cohort's
//! signed records verified in the private bucket and its allow-listed
//! summary ([`crate::summary`]) was written. Artifact states inside an entry
//! advance across runs as copies finish. Schema v2 carries no score, count
//! or record of the hidden evaluation: no mode, package or closure digest,
//! receipt count or phase epochs between the opening and the close.

use serde::{Deserialize, Serialize};
use slop_ninja_admission_wire::json::Chain;
use slop_ninja_protocol::{CalculationTargetV2, ChainBinding, CutoffMode, Record};

use crate::layout;
use crate::verify::VerifiedCohort;

pub const INDEX_SCHEMA: &str = "slop_ninja.public.cohort_index/v2";
/// The schema of the index layout v1 wrote, which listed the signed records
/// and receipt counts; a v2 publisher replaces it (`publish::Run::cohorts`).
pub const INDEX_SCHEMA_V1: &str = "slop_ninja.public.cohort_index/v1";
pub const PUBLICATION_SCHEMA: &str = "slop_ninja.public.publication/v2";
pub const REFERENCE_SCHEMA: &str = "slop_ninja.public.reference/v1";

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ArtifactState {
    /// Copied and checked against the package's SHA-256.
    Published,
    /// Never published as an entrant artifact; the reason says why.
    Withheld,
    /// Not copied yet; retried on later runs.
    Pending,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArtifactEntry {
    pub uid: u16,
    pub hotkey: String,
    /// SHA-256 of the artifact object, as the score package binds it.
    pub sha256: String,
    /// Canonical inference-content digest.
    pub content: String,
    pub state: ArtifactState,
    pub path: Option<String>,
    pub reason: Option<String>,
    /// The entrant's identity and opening as the signed package and closure
    /// state them, so a later run can check the admission record of a
    /// pending artifact without re-reading the cohort.
    pub hotkey_hex: String,
    pub registration_block: u64,
    pub commitment: String,
    pub nonce: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Publication {
    pub schema: String,
    pub cohort: u64,
    pub label: String,
    pub publisher: String,
    /// SHA-256 of the cohort's `summary.json`, the only cohort object
    /// published.
    pub summary: String,
    pub artifacts: Vec<ArtifactEntry>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CohortEntry {
    pub cohort: u64,
    pub label: String,
    pub path: String,
    pub opened_epoch: u64,
    pub closed_epoch: u64,
    /// The entrant promoted to `R_k+1`, if any.
    pub promoted: Option<String>,
    /// The target the cohort's closure activated, if any.
    pub target: Option<String>,
    pub reference_index: u64,
    /// Digest of the signed manifest (kept private), which coordinator rows
    /// and authorizations name.
    pub manifest: String,
    /// SHA-256 of the cohort's `summary.json`.
    pub summary: String,
    pub entrants: u32,
    pub artifacts_published: u32,
    pub artifacts_withheld: u32,
    pub artifacts_pending: u32,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TrustRef {
    pub path: String,
    pub sha256: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActiveTarget {
    pub digest: String,
    /// `launch_burn`, or the cohort whose target activated last.
    pub source: String,
    pub cohort: Option<u64>,
}

/// An initial reference (`R_0`, or its single replacement) some listed
/// cohort's manifest named. It is published as a reference package, never as
/// an entrant artifact, so its artifacts stay withheld as entrant artifacts
/// for every later cohort, after the manifest chain no longer names it.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct InitialReference {
    pub content: String,
    pub artifact: String,
    /// The first listed cohort that named it.
    pub first_cohort: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CohortIndex {
    pub schema: String,
    pub network: String,
    pub chain: Chain,
    pub trust: TrustRef,
    pub active_target: ActiveTarget,
    /// Ascending by cohort.
    pub cohorts: Vec<CohortEntry>,
    /// Every initial reference a listed cohort named.
    #[serde(default)]
    pub initial_references: Vec<InitialReference>,
    /// The record's label prefix (`RECORD_LABEL_PREFIX`) as this index
    /// adopted it; `None` before one was configured. A configured prefix
    /// that a listed cohort's label lacks starts a new record
    /// ([`record_change`]).
    #[serde(default)]
    pub record_prefix: Option<String>,
    /// The earlier public records moved aside when a new one started, oldest
    /// first: their prefixes are never adopted again.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub previous_records: Vec<ArchivedRecord>,
    /// Closed cohorts of this record whose detailed receipts the coordinator
    /// deleted under the N+2 rule before this publisher copied them (its
    /// `receipts-retired.json` marker): never listed, so the gap in the
    /// record is explained here. Their signed manifest, package and closure
    /// stay in the private bucket.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub retired_unpublished: Vec<RetiredCohort>,
    /// Closed cohorts of this record the publisher build `build` refused
    /// (a signed object that does not verify, a numbering conflict, the
    /// memory budget): content free, so the gap in the record is visible and
    /// later cohorts are listed after it. A later build reconsiders each; one
    /// that accepts it lists it in its place by number and removes the entry.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub refused: Vec<RefusedCohort>,
}

/// A closed cohort the publisher refused: never copied, explained here.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RefusedCohort {
    pub label: String,
    /// From its signed manifest, when that opened.
    pub cohort: Option<u64>,
    pub opened_epoch: Option<u64>,
    /// Its manifest starts the record (reference index 0, no predecessor):
    /// later cohorts are listed after it although it is not, until a later
    /// build lists it in its place (`publish::record_start`). `false` when
    /// its manifest did not open.
    #[serde(default)]
    pub starts_record: bool,
    /// The refusal's content-free public reason: the object and the kind of
    /// failure only (`publish::public_reason`); the full reason stays in the
    /// alert.
    pub reason: String,
    /// The publisher build that refused it (`publish::build_id`).
    pub build: String,
}

/// A closed cohort whose detailed receipts were deleted before publication.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RetiredCohort {
    pub label: String,
    /// From its signed manifest.
    pub cohort: u64,
    pub opened_epoch: u64,
    /// The coordinator's completion after which they were deleted.
    pub due_after_cohort: u64,
    /// Its manifest starts the record (reference index 0, no predecessor):
    /// later cohorts are listed after it although it is not
    /// (`publish::record_start`).
    #[serde(default)]
    pub starts_record: bool,
}

/// An earlier public record, moved under `v1/records/<first_label>/` when a
/// new record started (a chain-record restart). The archive is the record's
/// index as it stood then, written once and never rewritten. An archive of
/// layout v1 (whose fields the allow-list no longer accepts) is deleted when
/// the v1 index is migrated (`publish::Run::cohorts`); this entry, its
/// prefix and initial references, stays and keeps doing its work.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArchivedRecord {
    /// The prefix it was built under (`None`: before one was configured).
    pub prefix: Option<String>,
    /// Its first listed cohort's label, the archive's directory. A
    /// configured prefix this label starts with names this record too, even
    /// when it had none recorded ([`ArchivedRecord::named_by`]).
    pub first_label: String,
    /// Its initial references (its index's `initial_references`): their
    /// artifacts stay withheld as entrant artifacts in every later record,
    /// whose manifests never name them.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub initial_references: Vec<InitialReference>,
}

impl ArchivedRecord {
    /// Whether the record prefix `p` names this earlier record: its own
    /// recorded prefix, or a prefix its first label has (a record moved aside
    /// before its index recorded one).
    pub fn named_by(&self, p: &str) -> bool {
        self.prefix.as_deref() == Some(p) || self.first_label.starts_with(p)
    }
}

/// What the configured record prefix means for the stored index.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RecordChange {
    /// The index is this record's; `Some` names a prefix it should adopt.
    Same(Option<String>),
    /// The index lists cohorts outside the configured record (a chain-record
    /// restart under a new authority): it is moved aside and a fresh record
    /// starts.
    Restart,
    /// The configured prefix is an earlier record's, already moved aside: a
    /// deploy from a tree that still tracks it. Nothing is moved or adopted;
    /// the run publishes nothing and reports why.
    Refused(String),
}

/// Whether the index `idx` belongs to the record whose labels start with
/// `prefix`. No prefix configured never restarts a record (the epoch bound
/// alone may move to reconsider skipped cohorts); a prefix that names an
/// earlier record ([`ArchivedRecord::named_by`]) is refused; a prefix every
/// listed label has, or an empty index, is adopted; a listed label without it
/// restarts.
pub fn record_change(idx: &CohortIndex, prefix: Option<&str>) -> RecordChange {
    let Some(p) = prefix else {
        return RecordChange::Same(None);
    };
    if idx.previous_records.iter().any(|r| r.named_by(p)) {
        return RecordChange::Refused(format!(
            "RECORD_LABEL_PREFIX {p} names an earlier public record, already moved aside; \
             deploy the publisher from a tree that tracks the current record"
        ));
    }
    if idx.cohorts.iter().all(|c| c.label.starts_with(p)) {
        let adopt = (idx.record_prefix.as_deref() != Some(p)).then(|| p.to_owned());
        RecordChange::Same(adopt)
    } else {
        RecordChange::Restart
    }
}

/// The publisher's pre-deploy check (`slop_ninja_publisher_ops
/// record-check`): `Ok` with the line to print, or why a tree tracking
/// `RECORD_LABEL_PREFIX` `tracked` must not be deployed over the live index
/// `idx`. Without `restart`, the live record must be the tracked one: its
/// adopted prefix equals it, or (an index written before prefixes were
/// recorded) every listed label has it. With `restart` (the publisher commit
/// of a chain-record restart only), the tracked prefix must be new: neither
/// the live record's nor an earlier one's.
pub fn record_check(idx: &CohortIndex, tracked: &str, restart: bool) -> Result<String, String> {
    if idx.schema != INDEX_SCHEMA {
        return Err("not a public cohort index".into());
    }
    if tracked.is_empty() {
        return Err("the tree tracks no RECORD_LABEL_PREFIX".into());
    }
    let earlier = idx.previous_records.iter().any(|r| r.named_by(tracked));
    let live = idx.record_prefix.as_deref();
    if restart {
        if earlier || live == Some(tracked) {
            return Err(format!(
                "{tracked} is the live or an earlier record's prefix, not a new one"
            ));
        }
        return Ok(format!(
            "record restart: {} -> {tracked}",
            live.unwrap_or("(no prefix recorded)")
        ));
    }
    if earlier {
        return Err(format!(
            "{tracked} names an earlier record, already moved aside"
        ));
    }
    match live {
        Some(p) if p == tracked => Ok(format!("same record: {tracked}")),
        Some(p) => Err(format!("the live record is {p}; the tree tracks {tracked}")),
        None if idx.cohorts.iter().all(|c| c.label.starts_with(tracked)) => Ok(format!(
            "the live index records no prefix and every listed label has {tracked}: it is adopted"
        )),
        None => Err(format!(
            "the live index records no prefix and lists labels without {tracked}"
        )),
    }
}

pub fn mode_name(mode: CutoffMode) -> &'static str {
    match mode {
        CutoffMode::Margin => "margin",
        CutoffMode::Fallback => "fallback",
        CutoffMode::NoTarget => "no_target",
    }
}

impl CohortIndex {
    pub fn empty(chain: &ChainBinding) -> Self {
        let mut index = Self {
            schema: INDEX_SCHEMA.into(),
            network: "testnet".into(),
            chain: Chain::from_binding(chain),
            trust: TrustRef {
                path: layout::TRUST.into(),
                sha256: slop_ninja_protocol::object_sha256(crate::config::TRUST_FILE).hex(),
            },
            active_target: ActiveTarget {
                digest: String::new(),
                source: "launch_burn".into(),
                cohort: None,
            },
            cohorts: Vec::new(),
            initial_references: Vec::new(),
            record_prefix: None,
            previous_records: Vec::new(),
            retired_unpublished: Vec::new(),
            refused: Vec::new(),
        };
        index.refresh(chain);
        index
    }

    /// Records the initial reference `v`'s manifest names, if any.
    pub fn note_initial_reference(&mut self, v: &VerifiedCohort) {
        let Some(r) = initial_reference(&v.manifest.references) else {
            return;
        };
        let (content, artifact) = (r.content.hex(), r.artifact.hex());
        match self
            .initial_references
            .iter_mut()
            .find(|x| x.content == content && x.artifact == artifact)
        {
            Some(x) => x.first_cohort = x.first_cohort.min(v.manifest.cohort),
            None => self.initial_references.push(InitialReference {
                content,
                artifact,
                first_cohort: v.manifest.cohort,
            }),
        }
        self.initial_references.sort();
    }

    /// Every initial reference whose artifacts stay withheld as entrant
    /// artifacts: the ones this record's cohorts named and every earlier
    /// record's ([`ArchivedRecord::initial_references`]), so a record restart
    /// keeps them withheld whatever the publisher's memo holds.
    pub fn withheld_initial_references(&self) -> impl Iterator<Item = &InitialReference> {
        self.initial_references.iter().chain(
            self.previous_records
                .iter()
                .flat_map(|r| r.initial_references.iter()),
        )
    }

    pub fn contains(&self, label: &str) -> bool {
        self.cohorts.iter().any(|c| c.label == label)
    }

    /// Records (or replaces) cohort `r.label`'s refusal.
    pub fn note_refused(&mut self, r: RefusedCohort) {
        self.refused.retain(|x| x.label != r.label);
        self.refused.push(r);
        self.refused.sort_by(|a, b| {
            (a.cohort, a.opened_epoch, &a.label).cmp(&(b.cohort, b.opened_epoch, &b.label))
        });
    }

    /// Drops cohort `label`'s refusal (it was listed, retired or placed
    /// outside the record since). `true` when there was one.
    pub fn clear_refused(&mut self, label: &str) -> bool {
        let before = self.refused.len();
        self.refused.retain(|x| x.label != label);
        self.refused.len() != before
    }

    /// Every cohort number this record knows completed at the coordinator:
    /// listed, retired before publication, or refused here (a refused
    /// cohort's closure exists all the same). Ascending, without repeats.
    pub fn completions(&self) -> Vec<u64> {
        let mut out: Vec<u64> = self
            .cohorts
            .iter()
            .map(|c| c.cohort)
            .chain(self.retired_unpublished.iter().map(|r| r.cohort))
            .chain(self.refused.iter().filter_map(|r| r.cohort))
            .collect();
        out.sort_unstable();
        out.dedup();
        out
    }

    pub fn last_cohort(&self) -> Option<u64> {
        self.cohorts.last().map(|c| c.cohort)
    }

    /// Inserts or replaces an entry and recomputes the derived fields.
    pub fn upsert(&mut self, entry: CohortEntry, chain: &ChainBinding) {
        self.cohorts.retain(|c| c.label != entry.label);
        self.cohorts.push(entry);
        self.cohorts
            .sort_by(|a, b| (a.cohort, &a.label).cmp(&(b.cohort, &b.label)));
        self.refresh(chain);
    }

    /// The active target: the last closed cohort that produced one (a later
    /// cohort without a target carries it), else the launch burn row.
    fn refresh(&mut self, chain: &ChainBinding) {
        self.trust = TrustRef {
            path: layout::TRUST.into(),
            sha256: slop_ninja_protocol::object_sha256(crate::config::TRUST_FILE).hex(),
        };
        self.active_target = match self.cohorts.iter().rev().find(|c| c.target.is_some()) {
            Some(c) => ActiveTarget {
                digest: c.target.clone().unwrap_or_default(),
                source: "cohort".into(),
                cohort: Some(c.cohort),
            },
            None => ActiveTarget {
                digest: CalculationTargetV2::launch_burn(chain.clone())
                    .digest()
                    .map(|d| d.hex())
                    .unwrap_or_default(),
                source: "launch_burn".into(),
                cohort: None,
            },
        };
    }
}

/// Layout v1's index as a v2 publisher adopts it: its chain must be this
/// one; the record prefix, the initial references (still withheld as entrant
/// artifacts), the earlier records and the cohorts retired before
/// publication are kept. Its cohort entries are dropped: they named signed
/// records and receipt counts the public host no longer carries, and every
/// in-record cohort is listed again from its private records, one a run.
pub fn migrate_v1(v: &serde_json::Value, chain: &ChainBinding) -> Result<CohortIndex, String> {
    let bad = |what: &str| {
        format!("the v1 public index's {what} is not readable; refusing to replace it")
    };
    if v.get("chain")
        .cloned()
        .and_then(|c| serde_json::from_value::<Chain>(c).ok())
        != Some(Chain::from_binding(chain))
    {
        return Err(bad("chain"));
    }
    let mut idx = CohortIndex::empty(chain);
    let take = |key: &str| v.get(key).cloned().unwrap_or(serde_json::Value::Null);
    idx.record_prefix =
        serde_json::from_value(take("record_prefix")).map_err(|_| bad("record prefix"))?;
    if !take("initial_references").is_null() {
        idx.initial_references = serde_json::from_value(take("initial_references"))
            .map_err(|_| bad("initial references"))?;
    }
    if !take("previous_records").is_null() {
        idx.previous_records =
            serde_json::from_value(take("previous_records")).map_err(|_| bad("earlier records"))?;
    }
    if !take("retired_unpublished").is_null() {
        idx.retired_unpublished = serde_json::from_value(take("retired_unpublished"))
            .map_err(|_| bad("retired cohorts"))?;
    }
    Ok(idx)
}

/// A live index as the pre-deploy record check reads it: layout v2 as it
/// is, layout v1 (which a v2 publisher replaces at its first run) for its
/// record prefix, earlier records and listed labels only.
pub fn for_record_check(v: &serde_json::Value) -> Result<CohortIndex, String> {
    match v.get("schema").and_then(|s| s.as_str()) {
        Some(INDEX_SCHEMA) => {
            serde_json::from_value(v.clone()).map_err(|e| format!("not a public cohort index: {e}"))
        }
        Some(INDEX_SCHEMA_V1) => {
            let chain = v
                .get("chain")
                .cloned()
                .and_then(|c| serde_json::from_value::<Chain>(c).ok())
                .and_then(|c| c.binding())
                .ok_or("not a public cohort index: chain")?;
            let mut idx = migrate_v1(v, &chain)?;
            for c in v
                .get("cohorts")
                .and_then(|c| c.as_array())
                .into_iter()
                .flatten()
            {
                let label = c
                    .get("label")
                    .and_then(|l| l.as_str())
                    .ok_or("not a public cohort index: label")?;
                idx.cohorts.push(CohortEntry {
                    cohort: c.get("cohort").and_then(|n| n.as_u64()).unwrap_or_default(),
                    label: label.to_owned(),
                    path: format!("{}/", layout::cohort_dir(label)),
                    opened_epoch: 0,
                    closed_epoch: 0,
                    promoted: None,
                    target: None,
                    reference_index: 0,
                    manifest: String::new(),
                    summary: String::new(),
                    entrants: 0,
                    artifacts_published: 0,
                    artifacts_withheld: 0,
                    artifacts_pending: 0,
                });
            }
            Ok(idx)
        }
        _ => Err("not a public cohort index".into()),
    }
}

/// The initial reference a manifest's reference set names: the current one
/// at index 0 (`R_0` or its replacement), its predecessor at index 1, else
/// none (later references are promoted miner artifacts).
pub fn initial_reference(
    refs: &slop_ninja_protocol::ReferenceSet,
) -> Option<slop_ninja_protocol::ReferenceModel> {
    match (refs.index, &refs.predecessor) {
        (0, _) => Some(refs.current),
        (1, Some(p)) => Some(*p),
        _ => None,
    }
}

/// The index entry of a verified cohort with its summary's SHA-256 and its
/// artifact states.
pub fn entry(v: &VerifiedCohort, summary: &str, artifacts: &[ArtifactEntry]) -> CohortEntry {
    let digest = |d: Result<slop_ninja_protocol::Digest, _>| d.map(|d| d.hex()).unwrap_or_default();
    let count = |s: ArtifactState| artifacts.iter().filter(|a| a.state == s).count() as u32;
    CohortEntry {
        cohort: v.manifest.cohort,
        label: v.label.clone(),
        path: format!("{}/", layout::cohort_dir(&v.label)),
        opened_epoch: v.manifest.opened_epoch,
        closed_epoch: v.closure.closed_epoch,
        promoted: v.package.promoted.map(|h| crate::ss58::encode(&h)),
        target: v.closure.target.map(|d| d.hex()),
        reference_index: v.package.reference_index,
        manifest: digest(v.manifest.digest()),
        summary: summary.to_owned(),
        entrants: v.package.entrants.len() as u32,
        artifacts_published: count(ArtifactState::Published),
        artifacts_withheld: count(ArtifactState::Withheld),
        artifacts_pending: count(ArtifactState::Pending),
    }
}

/// One reference pointer.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReferencePointer {
    pub schema: String,
    /// `current` (`R_k`) or `previous` (`R_k-1`).
    pub role: String,
    pub index: u64,
    pub content: String,
    pub artifact: String,
    /// As of the close of this cohort.
    pub as_of_cohort: u64,
    /// The download, when the reference is a published miner artifact.
    pub path: Option<String>,
    pub withheld: Option<String>,
    /// The staged reference package's directory, when the reference is one
    /// (`R_0` and its replacement are published this way).
    #[serde(default)]
    pub package: Option<String>,
    /// The package's licence, with `package`.
    #[serde(default)]
    pub licence: Option<String>,
}

/// Why a reference pointer has neither a download nor a package yet.
pub const REFERENCE_NOT_STAGED: &str = "reference package not yet staged";

impl ReferencePointer {
    /// Links the staged reference package of this pointer's artifact.
    pub fn link_package(&mut self) {
        self.package = Some(layout::reference_package(&parse(&self.artifact)));
        self.licence = Some(layout::REFERENCE_PACKAGE_LICENCE.to_owned());
        self.withheld = None;
    }
}

/// The reference chain after `v` closed: a promotion makes the promoted
/// entrant `R_k+1` and the old `R_k` its predecessor. `published` says
/// whether an artifact SHA-256 is a published miner artifact, `staged`
/// whether it has a staged reference package.
pub fn references_after(
    v: &VerifiedCohort,
    published: impl Fn(&str) -> bool,
    staged: impl Fn(&str) -> bool,
) -> (ReferencePointer, Option<ReferencePointer>) {
    let refs = &v.manifest.references;
    let pointer = |role: &str, index: u64, content: String, artifact: String| {
        let path = published(&artifact).then(|| layout::artifact(&parse(&artifact)));
        let mut p = ReferencePointer {
            schema: REFERENCE_SCHEMA.into(),
            role: role.into(),
            index,
            withheld: path.is_none().then(|| REFERENCE_NOT_STAGED.to_owned()),
            content,
            artifact,
            as_of_cohort: v.manifest.cohort,
            path,
            package: None,
            licence: None,
        };
        if p.path.is_none() && staged(&p.artifact) {
            p.link_package();
        }
        p
    };
    let promoted = v
        .package
        .promoted
        .and_then(|h| v.package.entrants.iter().find(|e| e.identity.hotkey == h));
    match promoted {
        Some(e) => (
            pointer("current", refs.index + 1, e.content.hex(), e.artifact.hex()),
            Some(pointer(
                "previous",
                refs.index,
                refs.current.content.hex(),
                refs.current.artifact.hex(),
            )),
        ),
        None => (
            pointer(
                "current",
                refs.index,
                refs.current.content.hex(),
                refs.current.artifact.hex(),
            ),
            refs.predecessor.map(|p| {
                pointer(
                    "previous",
                    refs.index.saturating_sub(1),
                    p.content.hex(),
                    p.artifact.hex(),
                )
            }),
        ),
    }
}

fn parse(hex_digest: &str) -> slop_ninja_protocol::Digest {
    let mut out = [0u8; 32];
    if let Ok(b) = hex::decode(hex_digest)
        && b.len() == 32
    {
        out.copy_from_slice(&b);
    }
    slop_ninja_protocol::Digest(out)
}
