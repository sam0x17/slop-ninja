use crate::digest;
use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

pub const CONTRACT_SCHEMA: &str = "slop_ninja_a_numeric_contract_v2";
pub const AUDIT_SCHEMA: &str = "slop_ninja_a_numeric_export_audit_v1";
pub const MASK_POLICY: &str = "slop_ninja_a_explicit_availability_oov_fallback_pos_v2";
pub const SPLIT_POLICY: &str = "slop_ninja_a_fixed_source_group_ancestry_v1";
pub const LEAF_SEMANTIC_NAMESPACE: &str = "slop_ninja_a_persistent_leaf_semantics_v1";
pub const MAX_BLOCK_DIMENSIONS: usize = 16_384;

/// Opaque categorical identity. Its hexadecimal digits never become scalar features.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(transparent)]
pub struct StableId(pub String);

impl StableId {
    pub fn validate(&self) -> Result<()> {
        ensure!(
            self.0.len() == 64
                && self
                    .0
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)),
            "expected lowercase SHA-256 identity"
        );
        Ok(())
    }
    /// Called by the preprocessing boundary, not by the learned runner.
    /// Only persistent lexical/morphology semantic IDs belong here. Inventory
    /// snapshots, store offsets, Merkle paths and chart handles do not. A bulk
    /// handle without a stable semantic binding must remain unavailable.
    pub fn leaf(lexeme_id: &str, morphology_id: Option<&str>) -> Result<Self> {
        ensure!(!lexeme_id.is_empty(), "empty persistent lexical identity");
        ensure!(
            morphology_id.is_none_or(|s| !s.is_empty()),
            "empty morphology identity"
        );
        Ok(Self(digest(
            LEAF_SEMANTIC_NAMESPACE,
            &(lexeme_id, morphology_id),
        )?))
    }
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum Split {
    Train,
    Development,
    Calibration,
    Test,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
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
pub const CLASS_NAMES: [&str; 3] = ["human_only", "model_only", "mixed"];

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Coordinate {
    Leaf { leaf_id: StableId, pos_id: u32 },
    LeafBigram { left: StableId, right: StableId },
    TextDerived { feature_id: StableId },
    Grammar { feature_id: StableId },
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Normalization {
    Counts,
    PerOpportunity,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RowPolicy {
    CompletePairedDocuments,
    MaskedPairedDocuments,
}

/// A declaration by the exporter, not an attestation that this crate checked parses.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum GrammarEvidence {
    NativeCfg,
    ExternalLinguistic,
    Synthetic,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Assignment {
    pub source_group: StableId,
    pub split: Split,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Contract {
    pub schema: String,
    pub parser_identity: StableId,
    pub resolution_identity: StableId,
    pub projection_identity: StableId,
    pub inventory_identity: StableId,
    pub leaf_semantic_namespace: String,
    pub segmentation_identity: StableId,
    pub document_aggregation_identity: StableId,
    pub source_manifest_identity: StableId,
    pub grammar_evidence: GrammarEvidence,
    pub mask_policy: String,
    pub split_policy: String,
    pub assignments: Vec<Assignment>,
    /// POS zero is reserved for unknown. Other ordinals bind this frozen catalog.
    pub pos_catalog: Vec<StableId>,
    pub lexical_dimensions: usize,
    pub grammar_dimensions: usize,
    pub lexical_catalog: Vec<Coordinate>,
    pub grammar_catalog: Vec<Coordinate>,
    pub normalization: Normalization,
    pub row_policy: RowPolicy,
}
impl Contract {
    pub fn validate(&self) -> Result<()> {
        ensure!(
            self.schema == CONTRACT_SCHEMA
                && self.mask_policy == MASK_POLICY
                && self.split_policy == SPLIT_POLICY
                && self.leaf_semantic_namespace == LEAF_SEMANTIC_NAMESPACE,
            "unsupported numeric contract/policy"
        );
        for id in [
            &self.parser_identity,
            &self.resolution_identity,
            &self.projection_identity,
            &self.inventory_identity,
            &self.segmentation_identity,
            &self.document_aggregation_identity,
            &self.source_manifest_identity,
        ] {
            id.validate()?;
        }
        ensure!(
            !self.pos_catalog.is_empty(),
            "POS catalog must include unknown at index zero"
        );
        let mut pos = BTreeSet::new();
        for id in &self.pos_catalog {
            id.validate()?;
            ensure!(pos.insert(id), "duplicate POS identity");
        }
        ensure!(
            self.lexical_dimensions == self.lexical_catalog.len()
                && self.grammar_dimensions == self.grammar_catalog.len(),
            "catalog dimension mismatch"
        );
        ensure!(
            (1..=MAX_BLOCK_DIMENSIONS).contains(&self.lexical_dimensions)
                && (1..=MAX_BLOCK_DIMENSIONS).contains(&self.grammar_dimensions),
            "both blocks need 1..16384 fixed coordinates"
        );
        let mut coordinates = BTreeSet::new();
        for (grammar, catalog) in [
            (false, &self.lexical_catalog),
            (true, &self.grammar_catalog),
        ] {
            for key in catalog {
                ensure!(
                    coordinates.insert(serde_json::to_string(key)?),
                    "duplicate coordinate"
                );
                match key {
                    Coordinate::Leaf { leaf_id, pos_id } => {
                        leaf_id.validate()?;
                        ensure!(
                            !grammar && (*pos_id as usize) < self.pos_catalog.len(),
                            "invalid leaf block/POS"
                        );
                    }
                    Coordinate::LeafBigram { left, right } => {
                        left.validate()?;
                        right.validate()?;
                        ensure!(!grammar, "leaf bigram in grammar block");
                    }
                    Coordinate::TextDerived { feature_id } => {
                        feature_id.validate()?;
                        ensure!(!grammar, "text-derived coordinate in grammar block");
                    }
                    Coordinate::Grammar { feature_id } => {
                        feature_id.validate()?;
                        ensure!(grammar, "grammar coordinate in lexical block");
                    }
                }
            }
        }
        ensure!(
            !self.assignments.is_empty(),
            "missing frozen group assignments"
        );
        let mut groups = BTreeSet::new();
        for a in &self.assignments {
            a.source_group.validate()?;
            ensure!(
                groups.insert(&a.source_group),
                "group assigned more than once"
            );
        }
        ensure!(
            self.assignments
                .windows(2)
                .all(|p| p[0].source_group < p[1].source_group),
            "assignments must be sorted by opaque group identity"
        );
        Ok(())
    }
    pub fn identity(&self) -> Result<StableId> {
        self.validate()?;
        Ok(StableId(digest(CONTRACT_SCHEMA, self)?))
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct NumericBlock {
    /// Counts or nonnegative measurements. PerOpportunity divides these in Rust.
    pub values: Vec<f64>,
    pub opportunities: Vec<u64>,
    pub available: Vec<bool>,
}
impl NumericBlock {
    fn validate(&self, dimensions: usize) -> Result<()> {
        ensure!(
            self.values.len() == dimensions
                && self.opportunities.len() == dimensions
                && self.available.len() == dimensions,
            "numeric block shape mismatch"
        );
        for ((v, n), available) in self
            .values
            .iter()
            .zip(&self.opportunities)
            .zip(&self.available)
        {
            ensure!(
                v.is_finite() && *v >= 0.0,
                "non-finite/negative feature value"
            );
            ensure!(
                if *available {
                    *n > 0
                } else {
                    *v == 0.0 && *n == 0
                },
                "unavailable values need zero payload/denominator; measured values need opportunities"
            );
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct LexicalStatus {
    pub observed_tokens: u64,
    /// OOV and unresolved are disjoint. Neither is silently assigned a leaf.
    pub oov_tokens: u64,
    pub unresolved_tokens: u64,
    pub unknown_pos_tokens: u64,
    /// Generic fallback leaves are not dictionary leaves, even with known POS.
    /// This may overlap OOV; fallback does not require an OOV surface.
    pub fallback_tokens: u64,
    /// Fixed POS catalog order; index zero counts unknown-POS fallbacks.
    pub fallback_by_pos: Vec<u64>,
    pub pos_available: bool,
    pub complete: bool,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Coverage {
    pub covered_units: u64,
    pub total_units: u64,
}

/// The entire learned-runner payload: no text, source labels, IDs or provenance.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct NumericInput {
    pub lexical: NumericBlock,
    pub grammar: NumericBlock,
    pub lexical_status: LexicalStatus,
    pub grammar_coverage: Coverage,
}
impl NumericInput {
    pub fn validate(&self, contract: &Contract) -> Result<()> {
        self.lexical.validate(contract.lexical_dimensions)?;
        self.grammar.validate(contract.grammar_dimensions)?;
        let s = &self.lexical_status;
        ensure!(
            s.oov_tokens <= s.observed_tokens
                && s.unresolved_tokens <= s.observed_tokens - s.oov_tokens
                && s.unknown_pos_tokens <= s.observed_tokens,
            "invalid OOV/unresolved/POS counts"
        );
        ensure!(
            s.pos_available || s.unknown_pos_tokens == s.observed_tokens,
            "missing POS must be explicit unknown POS"
        );
        ensure!(
            s.fallback_by_pos.len() == contract.pos_catalog.len(),
            "fallback POS shape must match frozen POS catalog"
        );
        let fallback_sum = s
            .fallback_by_pos
            .iter()
            .try_fold(0_u64, |sum, n| sum.checked_add(*n))
            .ok_or_else(|| anyhow::anyhow!("fallback POS count overflow"))?;
        ensure!(
            fallback_sum == s.fallback_tokens && s.fallback_tokens <= s.observed_tokens,
            "fallback POS counts must sum to total and fit observed tokens"
        );
        ensure!(
            s.fallback_by_pos[0] <= s.unknown_pos_tokens
                && s.fallback_tokens - s.fallback_by_pos[0]
                    <= s.observed_tokens - s.unknown_pos_tokens,
            "fallback POS counts conflict with known/unknown POS token counts"
        );
        ensure!(
            self.grammar_coverage.covered_units <= self.grammar_coverage.total_units,
            "invalid grammar coverage"
        );
        ensure!(
            self.grammar_coverage.covered_units > 0 || self.grammar.available.iter().all(|v| !v),
            "grammar observations without covered units"
        );
        Ok(())
    }
    pub fn complete(&self) -> bool {
        self.lexical_status.complete
            && self.lexical_status.observed_tokens > 0
            && self.lexical.available.iter().all(|v| *v)
            && self.grammar.available.iter().all(|v| *v)
            && self.grammar_coverage.total_units > 0
            && self.grammar_coverage.covered_units == self.grammar_coverage.total_units
    }
}

/// Audit/training boundary only. The numeric runner never takes this type.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct NumericRow {
    pub id: StableId,
    pub source_group: StableId,
    pub text_sha256: StableId,
    pub normalized_text_sha256: StableId,
    pub lineage_root: StableId,
    pub ancestor_ids: Vec<StableId>,
    pub audit_receipt_sha256: StableId,
    pub split: Split,
    pub label: Option<Origin>,
    pub input: NumericInput,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ExportAudit {
    pub schema: String,
    pub contract_sha256: StableId,
    pub rows_sha256: StableId,
    pub exporter_identity: StableId,
    pub source_manifest_identity: StableId,
    /// All source groups used for vocabulary, scale, parser, rule or selection tuning.
    /// A fixed externally authored grammar can declare an empty list; do not omit
    /// campaign sources actually used to develop this particular frozen grammar.
    pub fitting_source_groups: Vec<StableId>,
}

pub fn rows_identity(rows: &[NumericRow]) -> Result<StableId> {
    let mut ordered: Vec<_> = rows.iter().collect();
    ordered.sort_by(|a, b| a.id.cmp(&b.id));
    Ok(StableId(digest("slop_ninja_a_numeric_rows_v1", &ordered)?))
}

/// Validate the declared exporter audit and all exact/normalized/ancestry split fences.
/// The caller supplies hashes; this does not pretend to recompute them from hidden text.
pub fn validate_export(
    contract: &Contract,
    rows: &[NumericRow],
    audit: &ExportAudit,
) -> Result<()> {
    contract.validate()?;
    ensure!(
        audit.schema == AUDIT_SCHEMA
            && audit.contract_sha256 == contract.identity()?
            && audit.rows_sha256 == rows_identity(rows)?
            && audit.source_manifest_identity == contract.source_manifest_identity,
        "export audit binding mismatch"
    );
    audit.exporter_identity.validate()?;
    let assignments: BTreeMap<_, _> = contract
        .assignments
        .iter()
        .map(|a| (&a.source_group, a.split))
        .collect();
    let mut fit = BTreeSet::new();
    for group in &audit.fitting_source_groups {
        group.validate()?;
        ensure!(
            assignments.get(group) == Some(&Split::Train) && fit.insert(group),
            "fitting exposure includes non-Train/unknown/repeated group"
        );
    }
    let mut ids = BTreeSet::new();
    let mut hashes = BTreeMap::new();
    let mut normalized = BTreeMap::new();
    let mut ancestry = BTreeMap::new();
    for row in rows {
        for id in [
            &row.id,
            &row.source_group,
            &row.text_sha256,
            &row.normalized_text_sha256,
            &row.lineage_root,
            &row.audit_receipt_sha256,
        ] {
            id.validate()?;
        }
        ensure!(ids.insert(&row.id), "duplicate row identity");
        ensure!(
            assignments.get(&row.source_group) == Some(&row.split),
            "row violates frozen group assignment"
        );
        row.input.validate(contract)?;
        // Duplicate text within one split is retained as a dependent observation;
        // across splits it is forbidden even if the declared groups differ.
        for (map, key) in [
            (&mut hashes, &row.text_sha256),
            (&mut normalized, &row.normalized_text_sha256),
        ] {
            if let Some(previous) = map.insert(key.clone(), row.split) {
                ensure!(
                    previous == row.split,
                    "exact/normalized text crosses splits"
                );
            }
        }
        let mut seen_ancestors = BTreeSet::new();
        for id in &row.ancestor_ids {
            id.validate()?;
            ensure!(
                id != &row.id && seen_ancestors.insert(id),
                "self/repeated ancestor"
            );
        }
        ensure!(
            if row.lineage_root == row.id {
                row.ancestor_ids.is_empty()
            } else {
                row.ancestor_ids.first() == Some(&row.lineage_root)
            },
            "ancestry must be ordered root to parent, or empty for a root row"
        );
        for id in std::iter::once(&row.id)
            .chain(std::iter::once(&row.lineage_root))
            .chain(row.ancestor_ids.iter())
        {
            if let Some((split, group)) =
                ancestry.insert(id.clone(), (row.split, row.source_group.clone()))
            {
                ensure!(
                    split == row.split && group == row.source_group,
                    "ancestry crosses a source group or split"
                );
            }
        }
    }
    let known: BTreeMap<_, _> = rows.iter().map(|r| (&r.id, r)).collect();
    for row in rows {
        for (position, id) in row.ancestor_ids.iter().enumerate() {
            if let Some(ancestor) = known.get(id) {
                ensure!(
                    ancestor.lineage_root == row.lineage_root
                        && ancestor.ancestor_ids == row.ancestor_ids[..position],
                    "inconsistent/cyclic ancestry among known rows"
                );
            }
        }
    }
    Ok(())
}
