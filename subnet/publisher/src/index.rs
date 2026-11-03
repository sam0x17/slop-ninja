//! The public cohort index (`v1/cohorts/index.json`), each cohort's
//! publication record (`v1/cohorts/<label>/publication.json`) and the
//! reference pointers (`v1/reference/{current,previous}.json`).
//!
//! The index is the publisher's durable state: a cohort is published exactly
//! when it has an entry, and an entry is written only after every public
//! object of the cohort was copied and the copies verified. Artifact states
//! inside an entry advance across runs as copies finish.

use serde::{Deserialize, Serialize};
use slop_ninja_admission_wire::json::Chain;
use slop_ninja_protocol::{CalculationTargetV2, ChainBinding, CutoffMode, Record};

use crate::layout;
use crate::verify::{PublicObject, VerifiedCohort};

pub const INDEX_SCHEMA: &str = "slop_ninja.public.cohort_index/v1";
pub const PUBLICATION_SCHEMA: &str = "slop_ninja.public.publication/v1";
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
    /// Every copied object, relative to the cohort directory, with the digest
    /// and length verified before and after the copy.
    pub objects: Vec<PublicObject>,
    pub receipts_verified: u64,
    /// Why `components.json` was not published, when it exists.
    pub components_withheld: Option<String>,
    pub artifacts: Vec<ArtifactEntry>,
    /// The later cohort whose publication completed N+2 and removed this
    /// cohort's detailed receipts (index, chunk map and bundles) from the
    /// public host; `None` while they are retained.
    #[serde(default)]
    pub receipts_deleted_after: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CohortEntry {
    pub cohort: u64,
    pub label: String,
    pub path: String,
    pub opened_epoch: u64,
    pub admission_closed_epoch: u64,
    pub published_epoch: u64,
    pub closed_epoch: u64,
    /// `margin`, `fallback` or `no_target`.
    pub mode: String,
    pub promoted: Option<String>,
    pub target: Option<String>,
    pub reference_index: u64,
    pub manifest: String,
    pub package: String,
    pub closure: String,
    pub entrants: u32,
    pub receipts: u64,
    /// Whether the detailed receipts are still published (deleted once two
    /// later cohorts are published).
    pub receipts_retained: bool,
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

    pub fn contains(&self, label: &str) -> bool {
        self.cohorts.iter().any(|c| c.label == label)
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

/// The index entry of a verified cohort with its artifact states.
pub fn entry(v: &VerifiedCohort, artifacts: &[ArtifactEntry]) -> CohortEntry {
    let digest = |d: Result<slop_ninja_protocol::Digest, _>| d.map(|d| d.hex()).unwrap_or_default();
    let count = |s: ArtifactState| artifacts.iter().filter(|a| a.state == s).count() as u32;
    CohortEntry {
        cohort: v.manifest.cohort,
        label: v.label.clone(),
        path: format!("{}/", layout::cohort_dir(&v.label)),
        opened_epoch: v.manifest.opened_epoch,
        admission_closed_epoch: v.closure.admission_closed_epoch,
        published_epoch: v.closure.published_epoch,
        closed_epoch: v.closure.closed_epoch,
        mode: mode_name(v.package.mode).into(),
        promoted: v.package.promoted.map(|h| crate::ss58::encode(&h)),
        target: v.closure.target.map(|d| d.hex()),
        reference_index: v.package.reference_index,
        manifest: digest(v.manifest.digest()),
        package: digest(v.package.digest()),
        closure: digest(v.closure.digest()),
        entrants: v.package.entrants.len() as u32,
        receipts: v.receipts,
        receipts_retained: v.receipts > 0,
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
