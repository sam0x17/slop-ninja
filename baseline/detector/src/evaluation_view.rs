//! Frozen observations over a complete, independently validated ancestry archive.
use crate::dataset::{self, Evidence, Origin, OriginRecord, Split, sha256};
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::Path,
};

pub const SCHEMA: &str = "slop_ninja_origin_evaluation_view_v1";

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecordRef {
    pub id: String,
    pub text_sha256: String,
}

impl From<&OriginRecord> for RecordRef {
    fn from(record: &OriginRecord) -> Self {
        Self {
            id: record.id.clone(),
            text_sha256: record.text_sha256.clone(),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Composer {
    pub id: String,
    pub model_id: String,
    pub model_revision: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Revision {
    pub id: String,
    pub parent_id: String,
    pub model_id: String,
    pub model_revision: String,
    pub same_model_as_parent: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RevisionMode {
    Unrevised,
    SameModel,
    CrossModel,
}

impl RevisionMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Unrevised => "unrevised",
            Self::SameModel => "same_model",
            Self::CrossModel => "cross_model",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Chain {
    pub composer: Composer,
    pub initial_profile_id: Option<String>,
    pub revisions: Vec<Revision>,
    pub revision_depth: usize,
    pub mode: RevisionMode,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Family {
    pub source_group: String,
    pub human: RecordRef,
    pub model: RecordRef,
    pub mixed: RecordRef,
    pub chain: Chain,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EvaluationView {
    pub schema: String,
    pub records_sha256: String,
    pub split: Split,
    pub families: Vec<Family>,
}

/// Explicit, score-independent input to the builder. Its ordering is immaterial.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Selection {
    pub source_group: String,
    pub human_id: String,
    pub model_id: String,
    pub mixed_id: String,
}

/// Safe artifact/report metadata. It contains no private record IDs or text.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Binding {
    pub sha256: String,
    pub records_sha256: String,
    pub split: Split,
    pub selected_rows: usize,
    pub source_groups: usize,
}

pub struct LoadedView {
    pub view: EvaluationView,
    pub sha256: String,
    pub selected: Vec<OriginRecord>,
    /// Every original record in this partition, including unselected ancestors.
    pub archive: Vec<OriginRecord>,
}

impl LoadedView {
    pub fn binding(&self) -> Binding {
        Binding {
            sha256: self.sha256.clone(),
            records_sha256: self.view.records_sha256.clone(),
            split: self.view.split,
            selected_rows: self.selected.len(),
            source_groups: self.view.families.len(),
        }
    }
}

fn derived_family(selection: &Selection, records: &[OriginRecord], split: Split) -> Result<Family> {
    let by_id: BTreeMap<_, _> = records.iter().map(|r| (r.id.as_str(), r)).collect();
    let get = |id: &str| -> Result<&OriginRecord> {
        let record = *by_id
            .get(id)
            .with_context(|| format!("Unknown selected record {id}"))?;
        ensure!(
            record.source_group == selection.source_group && record.split == Some(split),
            "Selected record {id} has the wrong family or partition"
        );
        Ok(record)
    };
    let human = get(&selection.human_id)?;
    let model = get(&selection.model_id)?;
    let mixed = get(&selection.mixed_id)?;
    ensure!(
        human.origin == Origin::HumanOnly
            && human.parent_id.is_none()
            && human.generation.is_none()
            && matches!(
                human.evidence,
                Evidence::HistoricalProxy | Evidence::DocumentedHuman
            ),
        "Selected human must be a documented human root"
    );
    ensure!(
        model.origin == Origin::ModelOnly
            && matches!(
                model.evidence,
                Evidence::RecordedModelGeneration | Evidence::RecordedModelRevision
            ),
        "Selected model must have recorded generation or revision provenance"
    );
    ensure!(
        mixed.origin == Origin::Mixed
            && mixed.evidence == Evidence::ModelEditOfHistoricalProxy
            && mixed.parent_id.as_deref() == Some(&human.id),
        "Selected mixed record must be a direct edit of the selected human root"
    );
    let mut cursor = model;
    let mut reverse_revisions = Vec::new();
    // Full archive validation has already checked cycles, parents, groups and splits.
    while cursor.evidence == Evidence::RecordedModelRevision {
        let parent_id = cursor
            .parent_id
            .as_deref()
            .context("Revision lacks parent")?;
        let parent = get(parent_id)?;
        let generation = cursor
            .generation
            .as_ref()
            .context("Revision lacks generation")?;
        let parent_generation = parent
            .generation
            .as_ref()
            .context("Revision parent lacks generation")?;
        reverse_revisions.push(Revision {
            id: cursor.id.clone(),
            parent_id: parent.id.clone(),
            model_id: generation.model_id.clone(),
            model_revision: generation.model_revision.clone(),
            same_model_as_parent: generation.model_revision == parent_generation.model_revision,
        });
        cursor = parent;
    }
    ensure!(
        cursor.evidence == Evidence::RecordedModelGeneration
            && cursor.origin == Origin::ModelOnly
            && cursor.parent_id.as_deref() == Some(&human.id),
        "Revision chain must start at a recorded draft of the selected human root"
    );
    let composer = cursor
        .generation
        .as_ref()
        .context("Composer lacks generation")?;
    let edit = mixed
        .generation
        .as_ref()
        .context("Mixed record lacks generation")?;
    ensure!(
        composer.model_id == edit.model_id
            && composer.model_revision == edit.model_revision
            && composer.prompt_profile == edit.prompt_profile,
        "Selected edit is not paired with the original composer and frozen profile provenance"
    );
    reverse_revisions.reverse();
    let mode = if reverse_revisions.is_empty() {
        RevisionMode::Unrevised
    } else if reverse_revisions.iter().all(|r| r.same_model_as_parent) {
        RevisionMode::SameModel
    } else {
        RevisionMode::CrossModel
    };
    Ok(Family {
        source_group: selection.source_group.clone(),
        human: human.into(),
        model: model.into(),
        mixed: mixed.into(),
        chain: Chain {
            composer: Composer {
                id: cursor.id.clone(),
                model_id: composer.model_id.clone(),
                model_revision: composer.model_revision.clone(),
            },
            initial_profile_id: composer
                .prompt_profile
                .as_ref()
                .map(|p| p.profile.id.clone()),
            revision_depth: reverse_revisions.len(),
            revisions: reverse_revisions,
            mode,
        },
    })
}

/// Derive all chain metadata from a validated archive; callers supply only IDs.
pub fn build_view(
    records: &[OriginRecord],
    records_sha256: String,
    split: Split,
    selections: &[Selection],
) -> Result<EvaluationView> {
    dataset::validate_records(records)?;
    ensure!(split != Split::Train, "Evaluation views cannot mask Train");
    ensure!(
        records_sha256.len() == 64 && records_sha256.bytes().all(|b| b.is_ascii_hexdigit()),
        "Invalid full archive SHA256"
    );
    let groups: BTreeSet<_> = records
        .iter()
        .filter(|r| r.split == Some(split))
        .map(|r| r.source_group.as_str())
        .collect();
    ensure!(!groups.is_empty(), "Empty evaluation partition");
    let selected_groups: BTreeSet<_> = selections.iter().map(|s| s.source_group.as_str()).collect();
    ensure!(
        selected_groups.len() == selections.len() && groups == selected_groups,
        "View must select exactly one trio for every source family in the partition"
    );
    let mut families = selections
        .iter()
        .map(|s| derived_family(s, records, split))
        .collect::<Result<Vec<_>>>()?;
    families.sort_by(|a, b| a.source_group.cmp(&b.source_group));
    Ok(EvaluationView {
        schema: SCHEMA.into(),
        records_sha256,
        split,
        families,
    })
}

/// Read and validate the entire archive before applying the frozen observation view.
pub fn load_view(path: &Path, records_path: &Path, expected_split: Split) -> Result<LoadedView> {
    let (records, records_hash) = read_archive(records_path)?;
    let bytes = fs::read(path)?;
    let view: EvaluationView = serde_json::from_slice(&bytes)?;
    ensure!(
        view.schema == SCHEMA
            && view.records_sha256 == records_hash
            && view.split == expected_split,
        "Evaluation view schema, archive hash or partition differs"
    );
    let selections: Vec<_> = view
        .families
        .iter()
        .map(|f| Selection {
            source_group: f.source_group.clone(),
            human_id: f.human.id.clone(),
            model_id: f.model.id.clone(),
            mixed_id: f.mixed.id.clone(),
        })
        .collect();
    let expected = build_view(&records, records_hash, expected_split, &selections)?;
    ensure!(
        view == expected,
        "Evaluation view ordering, text hashes, pairing or derived revision chain differs from its archive"
    );
    let by_id: BTreeMap<_, _> = records.iter().map(|r| (r.id.as_str(), r)).collect();
    let selected = view
        .families
        .iter()
        .flat_map(|f| [&f.human, &f.model, &f.mixed])
        .map(|r| by_id[r.id.as_str()].clone())
        .collect();
    Ok(LoadedView {
        view,
        sha256: sha256(bytes),
        selected,
        archive: records
            .into_iter()
            .filter(|r| r.split == Some(expected_split))
            .collect(),
    })
}

/// Parse and hash the same bytes, retaining the dataset's full ancestry checks.
pub fn read_archive(path: &Path) -> Result<(Vec<OriginRecord>, String)> {
    let bytes = fs::read(path)?;
    let records = std::str::from_utf8(&bytes)?
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(serde_json::from_str)
        .collect::<serde_json::Result<Vec<OriginRecord>>>()?;
    ensure!(!records.is_empty(), "Dataset is empty");
    dataset::validate_records(&records)?;
    Ok((records, sha256(bytes)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    // Invented protocol records only; no model calls or detector evidence.
    fn fixture(group: &str) -> Vec<OriginRecord> {
        let human: OriginRecord = serde_json::from_value(json!({
            "schema":dataset::RECORD_SCHEMA,"id":format!("{group}-human"),"source_group":group,
            "split":"development","origin":"human_only","evidence":"historical_proxy",
            "evidence_notes":"Invented ancestry protocol fixture, not real authorship evidence.",
            "text":format!("A violet jar holds the paper moon for {group}."),
            "text_sha256":sha256(format!("A violet jar holds the paper moon for {group}.")),
            "source":{"collection":"synthetic-contract-fixture","url":"fixture://view/root","version":"fixture-v1",
                "published_at":null,"author_ids":[],"raw_path":"","raw_sha256":sha256("fixture"),"extraction":"original-fixture"},
            "rights":{"license":"Synthetic-Original","evidence_url":"fixture://view/rights","evidence_sha256":sha256("fixture grant"),
                "attribution":"Slop Ninja synthetic fixture","commercial_training":true,"model_release":true,"external_evaluation":false,"redistribute_text":true},
            "parent_id":null,"generation":null
        })).unwrap();
        let mut records = vec![human.clone()];
        for (id, parent, origin, evidence, operation, model) in [
            (
                "draft",
                "human",
                Origin::ModelOnly,
                Evidence::RecordedModelGeneration,
                "draft",
                "model-a",
            ),
            (
                "mixed",
                "human",
                Origin::Mixed,
                Evidence::ModelEditOfHistoricalProxy,
                "edit",
                "model-a",
            ),
            (
                "first",
                "draft",
                Origin::ModelOnly,
                Evidence::RecordedModelRevision,
                "revise",
                "model-a",
            ),
            (
                "second",
                "first",
                Origin::ModelOnly,
                Evidence::RecordedModelRevision,
                "revise",
                "model-b",
            ),
        ] {
            let mut record = human.clone();
            record.id = format!("{group}-{id}");
            record.parent_id = Some(format!("{group}-{parent}"));
            record.origin = origin;
            record.evidence = evidence;
            record.text = format!("Invented {id} record in protocol family {group}.");
            record.text_sha256 = sha256(&record.text);
            record.generation = Some(serde_json::from_value(json!({
                "model_id":model,"response_model":model,"model_revision":format!("{model}@pinned"),
                "model_license":"Apache-2.0","model_license_url":"fixture://view/model-license","model_license_sha256":sha256("fixture grant"),
                "quantization":"fixture","runtime":"fixture","prompt":"Invented protocol instruction.","request_sha256":sha256("request"),
                "response_sha256":sha256("response"),"operation":operation,"created_at":"2026-01-01T00:00:00Z",
                "temperature":0.7,"seed":null,"max_tokens":100,"finish_reason":"stop"
            })).unwrap());
            records.push(record);
        }
        records
    }

    fn selection(group: &str, stage: &str) -> Selection {
        Selection {
            source_group: group.into(),
            human_id: format!("{group}-human"),
            model_id: format!("{group}-{stage}"),
            mixed_id: format!("{group}-mixed"),
        }
    }

    fn build(records: &[OriginRecord], selections: &[Selection]) -> Result<EvaluationView> {
        build_view(
            records,
            sha256("full archive fixture"),
            Split::Development,
            selections,
        )
    }

    #[test]
    fn complete_archive_supports_explicit_stages_and_exact_revision_paths() {
        let records = fixture("one");
        for (stage, depth, mode) in [
            ("draft", 0, RevisionMode::Unrevised),
            ("first", 1, RevisionMode::SameModel),
            ("second", 2, RevisionMode::CrossModel),
        ] {
            let view = build(&records, &[selection("one", stage)]).unwrap();
            let chain = &view.families[0].chain;
            assert_eq!((chain.revision_depth, chain.mode), (depth, mode));
            assert_eq!(chain.composer.id, "one-draft");
            assert_eq!(chain.initial_profile_id, None);
            if depth == 2 {
                assert_eq!(chain.revisions[0].id, "one-first");
                assert!(chain.revisions[0].same_model_as_parent);
                assert!(!chain.revisions[1].same_model_as_parent);
            }
        }
        let mut records = records;
        // A model alias does not make the same exact pinned weights cross-model.
        records[3].generation.as_mut().unwrap().model_id = "alias-for-a".into();
        assert_eq!(
            build(&records, &[selection("one", "first")])
                .unwrap()
                .families[0]
                .chain
                .mode,
            RevisionMode::SameModel
        );
    }

    #[test]
    fn omissions_duplicate_groups_wrong_pairs_and_incomplete_ancestry_fail() {
        let mut records = fixture("one");
        records.extend(fixture("two"));
        assert!(build(&records, &[selection("one", "second")]).is_err());
        assert!(
            build(
                &records,
                &[selection("one", "second"), selection("one", "first")]
            )
            .is_err()
        );
        let selections = [selection("two", "first"), selection("one", "second")];
        assert_eq!(
            build(&records, &selections).unwrap().families[0].source_group,
            "one"
        );
        let mut wrong = selections.clone();
        wrong[0].human_id = "one-human".into();
        assert!(build(&records, &wrong).is_err());
        records[2].generation.as_mut().unwrap().model_revision = "wrong-model@pinned".into();
        assert!(build(&records, &selections).is_err());
        let mut incomplete = fixture("one");
        incomplete.remove(1);
        assert!(build(&incomplete, &[selection("one", "second")]).is_err());
        let mut invalid_unselected = fixture("one");
        invalid_unselected[4].text.push_str(" changed without hash");
        assert!(build(&invalid_unselected, &[selection("one", "draft")]).is_err());
    }

    #[test]
    fn raw_archive_view_hashes_and_derived_metadata_are_all_bound() {
        let temp = tempfile::tempdir().unwrap();
        let archive = temp.path().join("records.jsonl");
        let path = temp.path().join("view.json");
        let records = fixture("one");
        dataset::write_records(&archive, &records).unwrap();
        let view = build_view(
            &records,
            sha256(fs::read(&archive).unwrap()),
            Split::Development,
            &[selection("one", "second")],
        )
        .unwrap();
        let bytes = serde_json::to_vec_pretty(&view).unwrap();
        fs::write(&path, &bytes).unwrap();
        let loaded = load_view(&path, &archive, Split::Development).unwrap();
        assert_eq!(loaded.archive.len(), 5);
        assert_eq!(
            loaded
                .selected
                .iter()
                .map(|r| r.id.as_str())
                .collect::<Vec<_>>(),
            ["one-human", "one-second", "one-mixed"]
        );
        assert_eq!(loaded.sha256, sha256(&bytes));
        assert_eq!(loaded.binding().selected_rows, 3);
        assert!(load_view(&path, &archive, Split::Calibration).is_err());
        let mut changed = view.clone();
        changed.families[0].chain.mode = RevisionMode::SameModel;
        fs::write(&path, serde_json::to_vec(&changed).unwrap()).unwrap();
        assert!(load_view(&path, &archive, Split::Development).is_err());
        changed = view.clone();
        changed.families[0].human.text_sha256 = sha256("other");
        fs::write(&path, serde_json::to_vec(&changed).unwrap()).unwrap();
        assert!(load_view(&path, &archive, Split::Development).is_err());
        fs::write(&path, &bytes).unwrap();
        use std::io::Write;
        fs::OpenOptions::new()
            .append(true)
            .open(&archive)
            .unwrap()
            .write_all(b"\n")
            .unwrap();
        assert!(load_view(&path, &archive, Split::Development).is_err());
    }

    #[test]
    fn manifest_family_order_and_unknown_fields_cannot_change_silently() {
        let temp = tempfile::tempdir().unwrap();
        let archive = temp.path().join("records.jsonl");
        let path = temp.path().join("view.json");
        let mut records = fixture("one");
        records.extend(fixture("two"));
        dataset::write_records(&archive, &records).unwrap();
        let mut view = build_view(
            &records,
            sha256(fs::read(&archive).unwrap()),
            Split::Development,
            &[selection("two", "second"), selection("one", "draft")],
        )
        .unwrap();
        view.families.reverse();
        fs::write(&path, serde_json::to_vec(&view).unwrap()).unwrap();
        assert!(load_view(&path, &archive, Split::Development).is_err());
        let mut value = serde_json::to_value(view).unwrap();
        value["predictions"] = json!([]);
        assert!(serde_json::from_value::<EvaluationView>(value).is_err());
    }
}
