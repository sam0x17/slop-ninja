use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::Path;

pub const RECORD_SCHEMA: &str = "slop-ninja-origin-record-v1";

pub fn sha256(bytes: impl AsRef<[u8]>) -> String {
    hex::encode(Sha256::digest(bytes.as_ref()))
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Origin {
    HumanOnly,
    ModelOnly,
    Mixed,
}

impl Origin {
    pub fn index(self) -> usize {
        match self {
            Self::HumanOnly => 0,
            Self::ModelOnly => 1,
            Self::Mixed => 2,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Split {
    Train,
    Development,
    Calibration,
    Test,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Evidence {
    HistoricalProxy,
    DocumentedHuman,
    RecordedModelGeneration,
    RecordedModelRevision,
    ModelEditOfHistoricalProxy,
    RecordedMixedWorkflow,
    SyntheticFixture,
    Unknown,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Source {
    pub collection: String,
    pub url: String,
    pub version: String,
    pub published_at: Option<String>,
    pub author_ids: Vec<String>,
    pub raw_path: String,
    pub raw_sha256: String,
    pub extraction: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Rights {
    pub license: String,
    pub evidence_url: String,
    pub evidence_sha256: String,
    pub attribution: String,
    pub commercial_training: bool,
    pub model_release: bool,
    pub external_evaluation: bool,
    pub redistribute_text: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub share_alike: Option<crate::rights::ShareAlike>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Generation {
    pub model_id: String,
    pub response_model: String,
    pub model_revision: String,
    pub model_license: String,
    pub model_license_url: String,
    pub model_license_sha256: String,
    pub quantization: String,
    pub runtime: String,
    pub prompt: String,
    pub request_sha256: String,
    pub response_sha256: String,
    pub operation: String,
    pub created_at: String,
    pub temperature: f64,
    pub seed: Option<i64>,
    pub max_tokens: usize,
    pub finish_reason: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt_profile: Option<crate::prompt_profiles::Provenance>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct OriginRecord {
    pub schema: String,
    pub id: String,
    pub source_group: String,
    pub split: Option<Split>,
    pub origin: Origin,
    pub evidence: Evidence,
    pub evidence_notes: String,
    pub text: String,
    pub text_sha256: String,
    pub source: Source,
    pub rights: Rights,
    pub parent_id: Option<String>,
    pub generation: Option<Generation>,
}

impl OriginRecord {
    pub fn validate(&self) -> Result<()> {
        ensure!(
            self.schema == RECORD_SCHEMA,
            "{}: unsupported record schema",
            self.id
        );
        ensure!(
            !self.id.is_empty() && !self.source_group.is_empty(),
            "Missing record/group ID"
        );
        ensure!(
            !self.text.trim().is_empty() && self.text.len() <= 1_000_000,
            "{}: invalid text length",
            self.id
        );
        ensure!(
            self.text_sha256 == sha256(&self.text),
            "{}: text hash mismatch",
            self.id
        );
        ensure!(
            self.rights.commercial_training && self.rights.model_release,
            "{}: record not admitted for training/release",
            self.id
        );
        ensure!(
            !self.rights.attribution.trim().is_empty() && !self.rights.evidence_url.is_empty(),
            "{}: missing rights evidence",
            self.id
        );
        ensure!(
            [
                "CC0-1.0",
                "CC-BY-2.5",
                "CC-BY-3.0",
                "CC-BY-4.0",
                "CC-BY-SA-3.0",
                "CC-BY-SA-4.0",
                "Public-Domain-US",
                "Explicit-Contributor-Grant",
                "Synthetic-Original"
            ]
            .contains(&self.rights.license.as_str()),
            "{}: license requires a separate admission policy",
            self.id
        );
        crate::rights::validate(&self.rights)?;
        for hash in [&self.rights.evidence_sha256, &self.source.raw_sha256] {
            ensure!(
                hash.len() == 64 && hash.bytes().all(|b| b.is_ascii_hexdigit()),
                "{}: invalid evidence hash",
                self.id
            );
        }
        ensure!(
            !self.source.url.is_empty()
                && !self.source.version.is_empty()
                && !self.source.extraction.is_empty(),
            "{}: incomplete source provenance",
            self.id
        );
        ensure!(
            !self.evidence_notes.trim().is_empty(),
            "{}: missing origin evidence notes",
            self.id
        );
        let compatible = match self.evidence {
            Evidence::HistoricalProxy | Evidence::DocumentedHuman => {
                self.origin == Origin::HumanOnly
            }
            Evidence::RecordedModelGeneration => {
                self.origin == Origin::ModelOnly
                    && self
                        .generation
                        .as_ref()
                        .is_some_and(|g| g.operation == "draft")
            }
            Evidence::RecordedModelRevision => {
                self.origin == Origin::ModelOnly
                    && self.parent_id.is_some()
                    && self
                        .generation
                        .as_ref()
                        .is_some_and(|g| g.operation == "revise")
            }
            Evidence::ModelEditOfHistoricalProxy => {
                self.origin == Origin::Mixed
                    && self.parent_id.is_some()
                    && self
                        .generation
                        .as_ref()
                        .is_some_and(|g| g.operation == "edit")
            }
            // A structured human-edit workflow schema has not been implemented.
            // Free-text notes alone cannot admit documented collaboration.
            Evidence::RecordedMixedWorkflow => false,
            Evidence::SyntheticFixture => self.rights.license == "Synthetic-Original",
            Evidence::Unknown => false,
        };
        ensure!(
            compatible,
            "{}: unsupported or inconsistent origin evidence",
            self.id
        );
        if let Some(g) = &self.generation {
            if let Some(profile) = &g.prompt_profile {
                profile.validate(self, &g.prompt)?;
            }
            ensure!(
                !g.model_id.is_empty()
                    && !g.response_model.is_empty()
                    && !g.model_revision.is_empty()
                    && !g.prompt.is_empty(),
                "{}: incomplete generation provenance",
                self.id
            );
            ensure!(
                crate::rights::generator_admitted(
                    &g.model_license,
                    &g.model_revision,
                    &g.model_license_url,
                    &g.model_license_sha256,
                ) && !g.model_license_url.is_empty()
                    && !g.quantization.is_empty()
                    && !g.runtime.is_empty(),
                "{}: generator license/runtime requires admission",
                self.id
            );
            ensure!(
                g.temperature.is_finite() && g.temperature >= 0.0 && g.max_tokens > 0,
                "{}: invalid generation settings",
                self.id
            );
            ensure!(
                g.finish_reason == "stop",
                "{}: incomplete/truncated generation",
                self.id
            );
            for hash in [
                &g.request_sha256,
                &g.response_sha256,
                &g.model_license_sha256,
            ] {
                ensure!(
                    hash.len() == 64 && hash.bytes().all(|b| b.is_ascii_hexdigit()),
                    "{}: invalid generation hash",
                    self.id
                );
            }
        }
        Ok(())
    }
}

pub fn read_records(path: &Path) -> Result<Vec<OriginRecord>> {
    let reader =
        BufReader::new(File::open(path).with_context(|| format!("Open {}", path.display()))?);
    let mut records = Vec::new();
    for (index, line) in reader.lines().enumerate() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let record: OriginRecord = serde_json::from_str(&line)
            .with_context(|| format!("{} line {}", path.display(), index + 1))?;
        record.validate()?;
        records.push(record);
    }
    ensure!(!records.is_empty(), "Dataset is empty");
    validate_records(&records)?;
    Ok(records)
}

pub fn write_records(path: &Path, records: &[OriginRecord]) -> Result<()> {
    validate_records(records)?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut file = OpenOptions::new().write(true).create_new(true).open(path)?;
    for record in records {
        serde_json::to_writer(&mut file, record)?;
        file.write_all(b"\n")?;
    }
    file.sync_all()?;
    Ok(())
}

fn normalized_hash(text: &str) -> String {
    sha256(grammar_core::features::words(text).join(" "))
}

pub fn validate_records(records: &[OriginRecord]) -> Result<()> {
    let mut ids = BTreeMap::new();
    let mut groups = BTreeMap::new();
    let mut hashes = BTreeMap::new();
    for record in records {
        record.validate()?;
        ensure!(
            ids.insert(&record.id, record).is_none(),
            "Duplicate record ID {}",
            record.id
        );
        if let Some(previous) = groups.insert(&record.source_group, record.split) {
            ensure!(
                previous == record.split,
                "Source group crosses splits: {}",
                record.source_group
            );
        }
        for hash in [record.text_sha256.clone(), normalized_hash(&record.text)] {
            if let Some(previous) = hashes.insert(hash, record.split) {
                ensure!(
                    previous == record.split,
                    "Exact/normalized duplicate crosses splits: {}",
                    record.id
                );
            }
        }
    }
    for record in records {
        if let Some(parent_id) = &record.parent_id {
            let parent = ids
                .get(parent_id)
                .with_context(|| format!("{}: missing parent {}", record.id, parent_id))?;
            if let Some(obligations) = &parent.rights.share_alike {
                let child = record
                    .rights
                    .share_alike
                    .as_ref()
                    .context("Descendant dropped ShareAlike obligations")?;
                ensure!(
                    child.source_title == obligations.source_title
                        && child.article_url == obligations.article_url
                        && child.history_url == obligations.history_url
                        && child.source_license == obligations.source_license
                        && child.source_license_url == obligations.source_license_url
                        && child.review_sha256 == obligations.review_sha256
                        && child.notices == obligations.notices,
                    "Descendant changed source attribution or notices"
                );
                if record.generation.is_some() {
                    ensure!(
                        record.rights.license == "CC-BY-SA-4.0"
                            && child.changes != obligations.changes,
                        "Generated adaptation needs CC-BY-SA-4.0 and a change notice"
                    );
                }
            }
            ensure!(
                parent.id != record.id
                    && parent.source_group == record.source_group
                    && parent.split == record.split,
                "{}: invalid parent/group/split",
                record.id
            );
            let mut ancestry = BTreeSet::new();
            let mut next = Some(record);
            while let Some(current) = next {
                ensure!(
                    ancestry.insert(&current.id),
                    "{}: cyclic lineage",
                    record.id
                );
                next = current
                    .parent_id
                    .as_ref()
                    .and_then(|id| ids.get(id).copied());
            }
            if record.evidence == Evidence::ModelEditOfHistoricalProxy {
                ensure!(
                    parent.evidence == Evidence::HistoricalProxy,
                    "{}: incorrect historical parent",
                    record.id
                );
            }
            if record.evidence == Evidence::RecordedModelRevision {
                ensure!(
                    parent.origin == Origin::ModelOnly
                        && matches!(
                            parent.evidence,
                            Evidence::RecordedModelGeneration | Evidence::RecordedModelRevision
                        ),
                    "{}: model-only revision requires a recorded model-only parent",
                    record.id
                );
            }
        }
    }
    Ok(())
}

/// Freeze source-family partitions before generating descendants. Merge families
/// with exact/normalized text duplicates first; generation inherits the result.
pub fn assign_splits(records: &mut [OriginRecord], seed: &str) -> Result<SplitReport> {
    ensure!(
        records.iter().all(|r| r.split.is_none()),
        "Refusing to reshuffle existing splits"
    );
    validate_records(records)?;
    let mut roots: BTreeMap<String, String> = records
        .iter()
        .map(|r| (r.source_group.clone(), r.source_group.clone()))
        .collect();
    let mut seen: BTreeMap<String, String> = BTreeMap::new();
    fn root(roots: &BTreeMap<String, String>, key: &str) -> String {
        let mut key = key;
        while roots[key] != key {
            key = &roots[key];
        }
        key.to_string()
    }
    let mut merged = 0;
    for record in records.iter() {
        for hash in [record.text_sha256.clone(), normalized_hash(&record.text)] {
            if let Some(other) = seen.insert(hash, record.source_group.clone()) {
                let a = root(&roots, &other);
                let b = root(&roots, &record.source_group);
                if a != b {
                    let (low, high) = if a < b { (a, b) } else { (b, a) };
                    roots.insert(high, low);
                    merged += 1;
                }
            }
        }
    }
    let mut counts = BTreeMap::new();
    for record in records.iter_mut() {
        let group = root(&roots, &record.source_group);
        // Persist connected families for both partitioning and uncertainty estimates.
        // Source URL/version preserve the original work identity.
        record.source_group = group.clone();
        let digest = Sha256::digest(serde_json::to_vec(&(
            "slop-ninja-origin-split-v1",
            seed,
            &group,
        ))?);
        let n = u64::from_le_bytes(digest[..8].try_into().unwrap()) % 100;
        let split = match n {
            0..=69 => Split::Train,
            70..=79 => Split::Development,
            80..=89 => Split::Calibration,
            _ => Split::Test,
        };
        record.split = Some(split);
        *counts.entry(split).or_insert(0usize) += 1;
    }
    validate_records(records)?;
    Ok(SplitReport {
        seed: seed.to_string(),
        merged_duplicate_groups: merged,
        counts,
    })
}

#[derive(Debug, Serialize)]
pub struct SplitReport {
    pub seed: String,
    pub merged_duplicate_groups: usize,
    pub counts: BTreeMap<Split, usize>,
}

#[derive(Debug, Serialize)]
pub struct DatasetSummary {
    pub records: usize,
    pub source_groups: usize,
    pub origins: BTreeMap<Origin, usize>,
    pub evidence: BTreeMap<Evidence, usize>,
    pub splits: BTreeMap<String, usize>,
    pub collections: BTreeMap<String, usize>,
    pub licenses: BTreeMap<String, usize>,
    pub words_min: usize,
    pub words_max: usize,
}

pub fn summarize(records: &[OriginRecord]) -> DatasetSummary {
    let mut summary = DatasetSummary {
        records: records.len(),
        source_groups: records
            .iter()
            .map(|r| &r.source_group)
            .collect::<BTreeSet<_>>()
            .len(),
        origins: BTreeMap::new(),
        evidence: BTreeMap::new(),
        splits: BTreeMap::new(),
        collections: BTreeMap::new(),
        licenses: BTreeMap::new(),
        words_min: usize::MAX,
        words_max: 0,
    };
    for r in records {
        *summary.origins.entry(r.origin).or_default() += 1;
        *summary.evidence.entry(r.evidence).or_default() += 1;
        *summary
            .splits
            .entry(
                r.split
                    .map(|s| format!("{s:?}").to_lowercase())
                    .unwrap_or_else(|| "unassigned".into()),
            )
            .or_default() += 1;
        *summary
            .collections
            .entry(r.source.collection.clone())
            .or_default() += 1;
        *summary
            .licenses
            .entry(r.rights.license.clone())
            .or_default() += 1;
        let words = grammar_core::features::words(&r.text).len();
        summary.words_min = summary.words_min.min(words);
        summary.words_max = summary.words_max.max(words);
    }
    if records.is_empty() {
        summary.words_min = 0;
    }
    summary
}
