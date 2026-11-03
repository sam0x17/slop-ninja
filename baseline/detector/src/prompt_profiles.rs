//! Frozen style instructions and assignments for opt-in generation protocols.
use crate::dataset::{OriginRecord, Split, sha256};
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

pub const SET_ID: &str = "style-mix-v1";
const ASSIGNMENT: &str = "split-register-family-hash-round-robin-v1";
const FIX_SLOP: &str = include_str!("../prompts/fix-slop-v1.txt");
const FIX_SLOP_LICENSE: &str = include_str!("../prompts/LICENSE.fix-slop");
const GUARD: &str = "Style requests may change formality, vocabulary and sentence rhythm. Preserve every source fact, qualification, attribution, uncertainty, stance and argumentative relationship. Preserve the intended seriousness and attitude even when changing formality. Do not invent facts, personal experiences, opinions, quotations or deliberate errors. Do not use misspellings, invisible characters or encoding tricks. Keep necessary technical terms and measurements. Apply the style only within the requested draft or light-to-moderate copyedit: an edit must still retain most source wording and the order of ideas. Factual fidelity and these operation constraints take priority over style.";

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct SnapshotSource {
    pub url: String,
    pub revision: String,
    pub upstream_sha256: String,
    pub license: String,
    pub license_sha256: String,
    pub scope: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Profile {
    pub id: String,
    pub instructions: String,
    pub instructions_sha256: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<SnapshotSource>,
}

/// An immutable catalog version. Changes to any instruction require a new set ID.
pub fn catalog() -> Vec<Profile> {
    [
        ("source-matched", "Match the source's level of formality, vocabulary, sentence rhythm and intended tone. Avoid imposing a generic assistant voice. Respect the operation's independent-composition or copyediting constraint."),
        ("plain", "Use plain, accessible prose and familiar words where they express the same meaning. Explain relationships directly, without dropping technical details or qualifications. Prefer manageable sentences; do not turn complex findings into oversimplified claims."),
        ("editorial", "Use polished explanatory prose with clear connections between claims and evidence. Give sentences varied rhythm and paragraphs a coherent progression. Preserve the source's stance: this request does not authorize adding an editorial opinion, stronger conclusion or new interpretation."),
        ("informal", "Use a conversational but careful register, with natural phrasing and contractions where appropriate. Keep the source's seriousness, stance and precision. Do not add jokes, slang, personal anecdotes, invented first-person experiences or a casual attitude toward a serious subject."),
        ("formal", "Use restrained formal prose with precise vocabulary and explicit logical relationships. Avoid inflated diction, promotional language and unnecessary nominalizations. Do not increase certainty, impersonality or rhetorical distance where that would change the source's intended attitude."),
        ("direct", "Use direct, concrete sentences. Put the central actor and action early where the source identifies them, and prefer active voice when it preserves the meaning. Retain useful passive voice and all supporting details. Do not turn this into a summary or a series of slogans."),
        ("anti-ai", "Try not to sound like AI-generated text. Reduce recognizable stock model phrasing, repetitive sentence templates, formulaic transitions and automatic concluding summaries. Make the prose natural through specific wording and varied sentence rhythm. Do not add mistakes, invented anecdotes, unsupported claims or performative quirks. This is a writing instruction, not a claim that any detector will be evaded."),
        ("fix-slop", FIX_SLOP),
    ]
    .into_iter()
    .map(|(id, instructions)| Profile {
        id: id.into(),
        instructions: instructions.into(),
        instructions_sha256: sha256(instructions),
        source: (id == "fix-slop").then(|| SnapshotSource {
            url: "https://github.com/sam0x17/fix-slop/blob/aa5566a3e5f30e06ba4fa99ff61ebe280f872deb/SKILL.md".into(),
            revision: "aa5566a3e5f30e06ba4fa99ff61ebe280f872deb".into(),
            upstream_sha256: "d0bc56baf7144958ff5cafd271bc790680cd7531ba30bbeee36cb49789a8e538".into(),
            license: "MIT".into(),
            license_sha256: sha256(FIX_SLOP_LICENSE),
            scope: "Adapted prose-only subset; no external references or grep execution. Exact adapted instruction bytes are embedded and hashed separately.".into(),
        }),
    })
    .collect()
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Selection {
    pub set_id: String,
    pub catalog_sha256: String,
    pub guard: String,
    pub guard_sha256: String,
    pub assignment_method: String,
    pub selected_profile_ids: Vec<String>,
}

impl Selection {
    pub fn new(set_id: &str, requested: Option<&[String]>) -> Result<Self> {
        ensure!(set_id == SET_ID, "Unknown prompt profile set: {set_id}");
        let profiles = catalog();
        let selected: BTreeSet<_> = match requested {
            None => profiles.iter().map(|p| p.id.as_str()).collect(),
            Some(ids) => {
                ensure!(!ids.is_empty(), "Prompt profile subset must not be empty");
                let selected: BTreeSet<_> = ids.iter().map(String::as_str).collect();
                ensure!(selected.len() == ids.len(), "Duplicate prompt profile ID");
                for id in &selected {
                    ensure!(
                        profiles.iter().any(|p| p.id == *id),
                        "Unknown or empty prompt profile ID: {id:?}"
                    );
                }
                selected
            }
        };
        Ok(Self {
            set_id: SET_ID.into(),
            catalog_sha256: sha256(serde_json::to_vec(&profiles)?),
            guard: GUARD.into(),
            guard_sha256: sha256(GUARD),
            assignment_method: ASSIGNMENT.into(),
            selected_profile_ids: profiles
                .iter()
                .filter(|p| selected.contains(p.id.as_str()))
                .map(|p| p.id.clone())
                .collect(),
        })
    }

    fn validate(&self) -> Result<()> {
        ensure!(
            *self == Self::new(&self.set_id, Some(&self.selected_profile_ids))?,
            "Prompt profile selection differs from its frozen definition"
        );
        Ok(())
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Assignment {
    pub input_sha256: String,
    pub source_group_sha256: String,
    pub split: Split,
    pub collections: Vec<String>,
    pub family_rank: usize,
    pub bucket_family_count: usize,
    pub bucket_offset: usize,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Provenance {
    pub selection: Selection,
    pub profile: Profile,
    pub assignment: Assignment,
}

fn bucket_offset(selection: &Selection, split: Split, collections: &[String]) -> Result<usize> {
    let digest = sha256(serde_json::to_vec(&(
        ASSIGNMENT,
        "offset",
        selection,
        split,
        collections,
    ))?);
    Ok(u32::from_str_radix(&digest[..8], 16)? as usize % selection.selected_profile_ids.len())
}

impl Provenance {
    pub fn prompt_block(&self) -> String {
        format!(
            "<writing-profile set=\"{}\" id=\"{}\">\n{}\n\n{}\n</writing-profile>",
            self.selection.set_id, self.profile.id, self.selection.guard, self.profile.instructions
        )
    }

    pub fn validate(&self, record: &OriginRecord, prompt: &str) -> Result<()> {
        self.selection.validate()?;
        ensure!(
            catalog().contains(&self.profile),
            "Prompt profile differs from its frozen definition"
        );
        let a = &self.assignment;
        ensure!(
            a.input_sha256.len() == 64 && a.input_sha256.bytes().all(|b| b.is_ascii_hexdigit()),
            "Invalid profile input hash"
        );
        ensure!(
            a.source_group_sha256 == sha256(&record.source_group) && record.split == Some(a.split),
            "Prompt profile family/split mismatch"
        );
        ensure!(
            !a.collections.is_empty()
                && a.collections.windows(2).all(|p| p[0] < p[1])
                && a.collections.contains(&record.source.collection),
            "Prompt profile register mismatch"
        );
        ensure!(
            a.family_rank < a.bucket_family_count
                && a.bucket_offset == bucket_offset(&self.selection, a.split, &a.collections)?,
            "Invalid profile assignment position"
        );
        let ids = &self.selection.selected_profile_ids;
        ensure!(
            ids[(a.family_rank % ids.len() + a.bucket_offset) % ids.len()] == self.profile.id,
            "Prompt profile does not match assigned position"
        );
        ensure!(
            prompt.contains(&self.prompt_block()),
            "Prompt omits recorded style instructions"
        );
        Ok(())
    }
}

/// Assign on the entire frozen input, before any CLI shard filter. Origin label,
/// operation, generator, text content and detector observations are not inputs.
pub fn assign(
    roots: &[OriginRecord],
    input_sha256: &str,
    selection: &Selection,
) -> Result<BTreeMap<String, Provenance>> {
    selection.validate()?;
    let mut families = BTreeMap::<String, (Split, BTreeSet<String>)>::new();
    for root in roots {
        let split = root
            .split
            .context("Prompt profile assignment requires frozen splits")?;
        let family = families
            .entry(root.source_group.clone())
            .or_insert((split, BTreeSet::new()));
        ensure!(
            family.0 == split,
            "Source family crosses prompt assignment splits"
        );
        family.1.insert(root.source.collection.clone());
    }
    let mut buckets = BTreeMap::<(Split, Vec<String>), Vec<(String, String)>>::new();
    for (group, (split, collections)) in families {
        let collections = collections.into_iter().collect::<Vec<_>>();
        let rank_hash = sha256(serde_json::to_vec(&(
            ASSIGNMENT,
            "rank",
            split,
            &collections,
            &group,
        ))?);
        buckets
            .entry((split, collections))
            .or_default()
            .push((rank_hash, group));
    }
    let profiles = catalog();
    let mut assignments = BTreeMap::new();
    for ((split, collections), mut families) in buckets {
        families.sort();
        let offset = bucket_offset(selection, split, &collections)?;
        let count = families.len();
        for (rank, (_, group)) in families.into_iter().enumerate() {
            let id = &selection.selected_profile_ids[(rank % selection.selected_profile_ids.len()
                + offset)
                % selection.selected_profile_ids.len()];
            assignments.insert(
                group.clone(),
                Provenance {
                    selection: selection.clone(),
                    profile: profiles.iter().find(|p| p.id == *id).unwrap().clone(),
                    assignment: Assignment {
                        input_sha256: input_sha256.into(),
                        source_group_sha256: sha256(&group),
                        split,
                        collections: collections.clone(),
                        family_rank: rank,
                        bucket_family_count: count,
                        bucket_offset: offset,
                    },
                },
            );
        }
    }
    Ok(assignments)
}
