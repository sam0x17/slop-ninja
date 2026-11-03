#![allow(dead_code)]
use grammar_a_baseline::*;

pub fn id(name: &str) -> StableId {
    StableId(digest("synthetic_fixture", &name).unwrap())
}
pub fn config() -> TrainConfig {
    TrainConfig {
        epochs: 60,
        patience: 15,
        ..TrainConfig::default()
    }
}
pub fn fixture() -> (Contract, Vec<NumericRow>, ExportAudit) {
    let mut assignments = Vec::new();
    let mut rows = Vec::new();
    for split in [
        Split::Train,
        Split::Development,
        Split::Calibration,
        Split::Test,
    ] {
        for family in 0..2 {
            let group = id(&format!("{split:?}-family-{family}"));
            let root = id(&format!("{split:?}-root-{family}"));
            assignments.push(Assignment {
                source_group: group.clone(),
                split,
            });
            for label in [Origin::HumanOnly, Origin::ModelOnly, Origin::Mixed] {
                let name = format!("{split:?}-{family}-{label:?}");
                let row_id = id(&name);
                let mut values = vec![0.0; 3];
                values[label.index()] = 10.0;
                let block = NumericBlock {
                    values,
                    opportunities: vec![10; 3],
                    available: vec![true; 3],
                };
                rows.push(NumericRow {
                    id: row_id.clone(),
                    source_group: group.clone(),
                    text_sha256: id(&format!("text-{name}")),
                    normalized_text_sha256: id(&format!("normalized-{name}")),
                    lineage_root: root.clone(),
                    ancestor_ids: vec![root.clone()],
                    audit_receipt_sha256: id(&format!("receipt-{name}")),
                    split,
                    label: Some(label),
                    input: NumericInput {
                        lexical: block.clone(),
                        grammar: block,
                        lexical_status: LexicalStatus {
                            observed_tokens: 10,
                            oov_tokens: 0,
                            unresolved_tokens: 0,
                            unknown_pos_tokens: 0,
                            fallback_tokens: 0,
                            fallback_by_pos: vec![0; 2],
                            pos_available: true,
                            complete: true,
                        },
                        grammar_coverage: Coverage {
                            covered_units: 1,
                            total_units: 1,
                        },
                    },
                });
            }
        }
    }
    assignments.sort_by(|a, b| a.source_group.cmp(&b.source_group));
    let contract = Contract {
        schema: CONTRACT_SCHEMA.into(),
        parser_identity: id("synthetic-parser"),
        resolution_identity: id("synthetic-resolution"),
        projection_identity: id("synthetic-projection"),
        inventory_identity: id("synthetic-inventory"),
        leaf_semantic_namespace: LEAF_SEMANTIC_NAMESPACE.into(),
        segmentation_identity: id("synthetic-segmentation"),
        document_aggregation_identity: id("synthetic-aggregation"),
        source_manifest_identity: id("synthetic-source-manifest"),
        grammar_evidence: GrammarEvidence::Synthetic,
        mask_policy: MASK_POLICY.into(),
        split_policy: SPLIT_POLICY.into(),
        assignments,
        pos_catalog: vec![id("unknown-pos"), id("noun-pos")],
        lexical_dimensions: 3,
        grammar_dimensions: 3,
        lexical_catalog: (0..3)
            .map(|n| Coordinate::TextDerived {
                feature_id: id(&format!("lexical-{n}")),
            })
            .collect(),
        grammar_catalog: (0..3)
            .map(|n| Coordinate::Grammar {
                feature_id: id(&format!("grammar-{n}")),
            })
            .collect(),
        normalization: Normalization::PerOpportunity,
        row_policy: RowPolicy::CompletePairedDocuments,
    };
    let train_rows = rows
        .iter()
        .filter(|r| r.split != Split::Test)
        .cloned()
        .collect::<Vec<_>>();
    let audit = audit(&contract, &train_rows);
    (contract, rows, audit)
}
pub fn audit(contract: &Contract, rows: &[NumericRow]) -> ExportAudit {
    ExportAudit {
        schema: AUDIT_SCHEMA.into(),
        contract_sha256: contract.identity().unwrap(),
        rows_sha256: rows_identity(rows).unwrap(),
        exporter_identity: id("synthetic-exporter"),
        source_manifest_identity: contract.source_manifest_identity.clone(),
        fitting_source_groups: contract
            .assignments
            .iter()
            .filter(|a| a.split == Split::Train)
            .map(|a| a.source_group.clone())
            .collect(),
    }
}
pub fn training(rows: &[NumericRow]) -> Vec<NumericRow> {
    rows.iter()
        .filter(|r| r.split != Split::Test)
        .cloned()
        .collect()
}
pub fn fit_ready(contract: &Contract, rows: &[NumericRow]) -> (Artifact, ComparisonReport) {
    match fit(contract, rows, &audit(contract, rows), &config()).unwrap() {
        FitOutcome::Ready { artifact, report } => (*artifact, report),
        other => panic!("expected synthetic fit: {other:?}"),
    }
}
pub fn unavailable(input: &mut NumericInput) {
    input.grammar.values.fill(0.0);
    input.grammar.opportunities.fill(0);
    input.grammar.available.fill(false);
    input.grammar_coverage.covered_units = 0;
}
