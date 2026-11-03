use crate::{contract::*, digest};
use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

pub const MODEL_SCHEMA: &str = "slop_ninja_a_numeric_softmax_v2";
pub const REPORT_SCHEMA: &str = "slop_ninja_a_numeric_comparison_v1";

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Mode {
    Lexical,
    Grammar,
    Combined,
}
pub const MODES: [Mode; 3] = [Mode::Lexical, Mode::Grammar, Mode::Combined];

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct TrainConfig {
    pub epochs: usize,
    pub patience: usize,
    pub learning_rate: f64,
    pub l2: f64,
    pub scale_floor: f64,
    pub min_groups_per_split: usize,
    pub min_rows_per_class: usize,
    /// Frozen before fitting. No automatic threshold selection on evaluation rows.
    pub abstain_below: Option<f64>,
}
impl Default for TrainConfig {
    fn default() -> Self {
        Self {
            epochs: 200,
            patience: 25,
            learning_rate: 0.03,
            l2: 0.01,
            scale_floor: 0.01,
            min_groups_per_split: 2,
            min_rows_per_class: 1,
            abstain_below: None,
        }
    }
}
impl TrainConfig {
    pub fn validate(&self) -> Result<()> {
        ensure!(
            (1..=10_000).contains(&self.epochs) && self.patience > 0,
            "invalid epoch/patience budget"
        );
        ensure!(
            self.learning_rate.is_finite()
                && self.learning_rate > 0.0
                && self.l2.is_finite()
                && self.l2 >= 0.0
                && self.scale_floor.is_finite()
                && self.scale_floor > 0.0,
            "invalid optimizer/scaling parameters"
        );
        ensure!(
            self.min_groups_per_split >= 1 && self.min_rows_per_class >= 1,
            "positive eligibility minimums required"
        );
        ensure!(
            self.abstain_below
                .is_none_or(|p| p.is_finite() && (0.0..=1.0).contains(&p)),
            "invalid fixed abstention threshold"
        );
        Ok(())
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Scales {
    pub lexical: Vec<f64>,
    pub grammar: Vec<f64>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Head {
    pub mode: Mode,
    pub weights: [Vec<f64>; 3],
    pub bias: [f64; 3],
    pub temperature: f64,
    pub selected_epoch: usize,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RowBinding {
    pub id: StableId,
    pub source_group: StableId,
    pub split: Split,
    pub text_sha256: StableId,
    pub normalized_text_sha256: StableId,
    pub lineage_root: StableId,
    pub ancestor_ids: Vec<StableId>,
}
impl From<&NumericRow> for RowBinding {
    fn from(r: &NumericRow) -> Self {
        Self {
            id: r.id.clone(),
            source_group: r.source_group.clone(),
            split: r.split,
            text_sha256: r.text_sha256.clone(),
            normalized_text_sha256: r.normalized_text_sha256.clone(),
            lineage_root: r.lineage_root.clone(),
            ancestor_ids: r.ancestor_ids.clone(),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ModelContent {
    pub schema: String,
    pub contract: Contract,
    pub contract_sha256: StableId,
    pub implementation_sha256: StableId,
    pub export_audit_sha256: StableId,
    pub training_rows_sha256: StableId,
    pub eligible_rows_sha256: StableId,
    pub classes: [String; 3],
    pub config: TrainConfig,
    pub scales: Scales,
    pub heads: [Head; 3],
    /// Includes excluded rows: they must not reappear in a later test split.
    pub fitting_bindings: Vec<RowBinding>,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Artifact {
    pub sha256: StableId,
    pub content: ModelContent,
}

fn width(contract: &Contract, mode: Mode) -> usize {
    let lexical = 2 * contract.lexical_dimensions + 7 + contract.pos_catalog.len();
    let grammar = 2 * contract.grammar_dimensions + 2;
    match mode {
        Mode::Lexical => lexical,
        Mode::Grammar => grammar,
        Mode::Combined => lexical + grammar,
    }
}

impl Artifact {
    pub fn validate(&self) -> Result<()> {
        let c = &self.content;
        c.contract.validate()?;
        c.config.validate()?;
        ensure!(
            c.schema == MODEL_SCHEMA && c.classes == CLASS_NAMES.map(str::to_owned),
            "unsupported model/class order"
        );
        ensure!(
            c.contract_sha256 == c.contract.identity()?
                && self.sha256 == StableId(digest(MODEL_SCHEMA, c)?),
            "model/contract digest mismatch"
        );
        for id in [
            &c.implementation_sha256,
            &c.export_audit_sha256,
            &c.training_rows_sha256,
            &c.eligible_rows_sha256,
        ] {
            id.validate()?;
        }
        for (values, n) in [
            (&c.scales.lexical, c.contract.lexical_dimensions),
            (&c.scales.grammar, c.contract.grammar_dimensions),
        ] {
            ensure!(
                values.len() == n
                    && values
                        .iter()
                        .all(|x| x.is_finite() && *x >= c.config.scale_floor),
                "invalid frozen scales"
            );
        }
        for (head, mode) in c.heads.iter().zip(MODES) {
            ensure!(
                head.mode == mode && head.selected_epoch <= c.config.epochs,
                "invalid head layout/epoch"
            );
            ensure!(
                head.temperature.is_finite()
                    && head.temperature > 0.0
                    && head.bias.iter().all(|x| x.is_finite()),
                "invalid head parameters"
            );
            for weights in &head.weights {
                ensure!(
                    weights.len() == width(&c.contract, mode)
                        && weights.iter().all(|x| x.is_finite()),
                    "invalid head weights"
                );
            }
        }
        ensure!(
            !c.fitting_bindings.is_empty()
                && c.fitting_bindings.iter().all(|r| r.split != Split::Test),
            "invalid fit ledger"
        );
        Ok(())
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct PartitionSummary {
    pub split: Split,
    pub supplied_rows: usize,
    pub eligible_rows: usize,
    pub groups: usize,
    pub class_rows: [usize; 3],
    pub unlabelled_rows: usize,
    pub incomplete_pair_rows: usize,
    pub grammar_unavailable_rows: usize,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Metrics {
    pub rows: usize,
    pub source_groups: usize,
    /// Each source family has equal total weight, rows within a family equal weight.
    pub family_log_loss: f64,
    pub family_brier: f64,
    pub family_accuracy: f64,
    pub row_accuracy: f64,
    pub confusion: [[usize; 3]; 3],
    pub class_recall: [Option<f64>; 3],
    pub accepted_rows: usize,
    pub selective_row_accuracy: Option<f64>,
    pub accepted_row_fraction: f64,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Epoch {
    pub epoch: usize,
    pub train_family_log_loss: f64,
    pub development_family_log_loss: f64,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct HeadReport {
    pub mode: Mode,
    pub parameters: usize,
    pub selected_epoch: usize,
    pub temperature: f64,
    pub history: Vec<Epoch>,
    pub development_uncalibrated: Metrics,
    pub calibration_uncalibrated: Metrics,
    pub calibration_fitted: Metrics,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ComparisonReport {
    pub schema: String,
    pub grammar_evidence: GrammarEvidence,
    pub partitions: Vec<PartitionSummary>,
    pub heads: Vec<HeadReport>,
    pub semantic_admission: bool,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
pub enum FitOutcome {
    Ready {
        artifact: Box<Artifact>,
        report: ComparisonReport,
    },
    Unavailable {
        reason: String,
        partitions: Vec<PartitionSummary>,
    },
}

fn eligible(contract: &Contract, row: &NumericRow) -> bool {
    row.label.is_some()
        && (contract.row_policy == RowPolicy::MaskedPairedDocuments || row.input.complete())
}
fn partition_summary(contract: &Contract, rows: &[NumericRow], split: Split) -> PartitionSummary {
    let supplied: Vec<_> = rows.iter().filter(|r| r.split == split).collect();
    let included: Vec<_> = supplied
        .iter()
        .copied()
        .filter(|r| eligible(contract, r))
        .collect();
    let mut classes = [0; 3];
    for r in &included {
        classes[r.label.unwrap().index()] += 1;
    }
    PartitionSummary {
        split,
        supplied_rows: supplied.len(),
        eligible_rows: included.len(),
        groups: included
            .iter()
            .map(|r| &r.source_group)
            .collect::<BTreeSet<_>>()
            .len(),
        class_rows: classes,
        unlabelled_rows: supplied.iter().filter(|r| r.label.is_none()).count(),
        incomplete_pair_rows: supplied.iter().filter(|r| !r.input.complete()).count(),
        grammar_unavailable_rows: supplied
            .iter()
            .filter(|r| r.input.grammar.available.iter().all(|v| !*v))
            .count(),
    }
}

/// Equal family weight, independent of the number of descendants retained per family.
pub fn source_family_weights(rows: &[&NumericRow]) -> Result<Vec<f64>> {
    ensure!(!rows.is_empty(), "no rows for source-family weighting");
    let mut groups = BTreeMap::new();
    for r in rows {
        *groups.entry(&r.source_group).or_insert(0usize) += 1;
    }
    Ok(rows
        .iter()
        .map(|r| 1.0 / (groups.len() as f64 * groups[&r.source_group] as f64))
        .collect())
}

fn normalized(block: &NumericBlock, mode: Normalization) -> Vec<f64> {
    block
        .values
        .iter()
        .zip(&block.opportunities)
        .zip(&block.available)
        .map(|((v, n), a)| {
            if !a {
                0.0
            } else {
                match mode {
                    Normalization::Counts => *v,
                    Normalization::PerOpportunity => *v / *n as f64,
                }
            }
        })
        .collect()
}

fn fit_scales(
    contract: &Contract,
    rows: &[&NumericRow],
    weights: &[f64],
    floor: f64,
) -> Result<Scales> {
    let mut result = Vec::new();
    for grammar in [false, true] {
        let n = if grammar {
            contract.grammar_dimensions
        } else {
            contract.lexical_dimensions
        };
        let mut sums = vec![0.0; n];
        let mut mass = vec![0.0; n];
        for (row, w) in rows.iter().zip(weights) {
            let block = if grammar {
                &row.input.grammar
            } else {
                &row.input.lexical
            };
            for (i, v) in normalized(block, contract.normalization).iter().enumerate() {
                if block.available[i] {
                    sums[i] += w * v * v;
                    mass[i] += w;
                }
            }
        }
        let scales: Vec<_> = sums
            .iter()
            .zip(&mass)
            .map(|(s, m)| {
                if *m > 0.0 {
                    (s / m).sqrt().max(floor)
                } else {
                    floor
                }
            })
            .collect();
        ensure!(scales.iter().all(|v| v.is_finite()), "scale overflow");
        result.push(scales);
    }
    Ok(Scales {
        lexical: result.remove(0),
        grammar: result.remove(0),
    })
}

fn feature_blocks(
    contract: &Contract,
    scales: &Scales,
    input: &NumericInput,
) -> Result<(Vec<f64>, Vec<f64>)> {
    let mut blocks = Vec::new();
    for (block, scale) in [
        (&input.lexical, &scales.lexical),
        (&input.grammar, &scales.grammar),
    ] {
        let mut x = Vec::with_capacity(block.values.len() * 2 + 6);
        for (v, s) in normalized(block, contract.normalization).iter().zip(scale) {
            x.push(v / s);
        }
        x.extend(block.available.iter().map(|v| if *v { 1.0 } else { 0.0 }));
        ensure!(x.iter().all(|v| v.is_finite()), "numeric feature overflow");
        blocks.push(x);
    }
    let mut grammar = blocks.pop().unwrap();
    let mut lexical = blocks.pop().unwrap();
    let s = &input.lexical_status;
    let ratio = |n: u64| {
        if s.observed_tokens > 0 {
            n as f64 / s.observed_tokens as f64
        } else {
            0.0
        }
    };
    lexical.extend([
        ratio(s.oov_tokens),
        ratio(s.unresolved_tokens),
        ratio(s.unknown_pos_tokens),
        if s.observed_tokens > 0 { 1.0 } else { 0.0 },
        if s.pos_available { 1.0 } else { 0.0 },
        if s.complete { 1.0 } else { 0.0 },
    ]);
    lexical.push(ratio(s.fallback_tokens));
    lexical.extend(s.fallback_by_pos.iter().map(|n| ratio(*n)));
    let c = &input.grammar_coverage;
    grammar.extend([
        if c.total_units > 0 {
            c.covered_units as f64 / c.total_units as f64
        } else {
            0.0
        },
        if c.total_units > 0 { 1.0 } else { 0.0 },
    ]);
    Ok((lexical, grammar))
}
fn features(
    contract: &Contract,
    scales: &Scales,
    input: &NumericInput,
    mode: Mode,
) -> Result<Vec<f64>> {
    let (mut lexical, grammar) = feature_blocks(contract, scales, input)?;
    Ok(match mode {
        Mode::Lexical => lexical,
        Mode::Grammar => grammar,
        Mode::Combined => {
            lexical.extend(grammar);
            lexical
        }
    })
}
fn probabilities(head: &Head, x: &[f64], temperature: f64) -> Result<[f64; 3]> {
    let mut logits = [0.0; 3];
    for (c, z) in logits.iter_mut().enumerate() {
        ensure!(
            head.weights[c].len() == x.len(),
            "inference dimension mismatch"
        );
        *z = (head.bias[c]
            + head.weights[c]
                .iter()
                .zip(x)
                .map(|(w, x)| w * x)
                .sum::<f64>())
            / temperature;
    }
    ensure!(
        logits.iter().all(|z| z.is_finite()),
        "non-finite inference logits"
    );
    let maximum = logits.iter().copied().fold(f64::NEG_INFINITY, f64::max);
    let exp = logits.map(|z| (z - maximum).exp());
    let total: f64 = exp.iter().sum();
    Ok(exp.map(|v| v / total))
}
fn loss(
    head: &Head,
    x: &[Vec<f64>],
    rows: &[&NumericRow],
    weights: &[f64],
    temperature: f64,
) -> Result<f64> {
    let mut total = 0.0;
    for ((x, row), w) in x.iter().zip(rows).zip(weights) {
        total -= w * probabilities(head, x, temperature)?[row.label.unwrap().index()]
            .max(1e-15)
            .ln();
    }
    ensure!(total.is_finite(), "non-finite weighted loss");
    Ok(total)
}
fn argmax(p: [f64; 3]) -> usize {
    let mut best = 0;
    for i in 1..3 {
        if p[i] > p[best] {
            best = i;
        }
    }
    best
}

fn metrics(
    head: &Head,
    x: &[Vec<f64>],
    rows: &[&NumericRow],
    temperature: f64,
    abstain: Option<f64>,
) -> Result<Metrics> {
    let weights = source_family_weights(rows)?;
    let mut brier = 0.0;
    let mut accuracy = 0.0;
    let mut correct = 0;
    let mut accepted = 0;
    let mut selective_correct = 0;
    let mut confusion = [[0; 3]; 3];
    for ((x, row), w) in x.iter().zip(rows).zip(&weights) {
        let p = probabilities(head, x, temperature)?;
        let label = row.label.unwrap().index();
        let predicted = argmax(p);
        let hit = usize::from(label == predicted);
        correct += hit;
        accuracy += w * hit as f64;
        confusion[label][predicted] += 1;
        brier += w * p
            .iter()
            .enumerate()
            .map(|(c, v)| (v - if c == label { 1.0 } else { 0.0 }).powi(2))
            .sum::<f64>();
        if abstain.is_none_or(|a| p[predicted] >= a) {
            accepted += 1;
            selective_correct += hit;
        }
    }
    let recall = std::array::from_fn(|c| {
        let n: usize = confusion[c].iter().sum();
        if n > 0 {
            Some(confusion[c][c] as f64 / n as f64)
        } else {
            None
        }
    });
    Ok(Metrics {
        rows: rows.len(),
        source_groups: rows
            .iter()
            .map(|r| &r.source_group)
            .collect::<BTreeSet<_>>()
            .len(),
        family_log_loss: loss(head, x, rows, &weights, temperature)?,
        family_brier: brier,
        family_accuracy: accuracy,
        row_accuracy: correct as f64 / rows.len() as f64,
        confusion,
        class_recall: recall,
        accepted_rows: accepted,
        selective_row_accuracy: if accepted > 0 {
            Some(selective_correct as f64 / accepted as f64)
        } else {
            None
        },
        accepted_row_fraction: accepted as f64 / rows.len() as f64,
    })
}

/// Deterministic full-batch Adam softmax, following the existing Rust origin
/// control's equations; this version accepts only frozen numeric feature blocks.
fn fit_head(
    contract: &Contract,
    scales: &Scales,
    train: &[&NumericRow],
    dev: &[&NumericRow],
    cal: &[&NumericRow],
    config: &TrainConfig,
    mode: Mode,
) -> Result<(Head, HeadReport)> {
    let project = |rows: &[&NumericRow]| {
        rows.iter()
            .map(|r| features(contract, scales, &r.input, mode))
            .collect::<Result<Vec<_>>>()
    };
    let train_x = project(train)?;
    let dev_x = project(dev)?;
    let cal_x = project(cal)?;
    let tw = source_family_weights(train)?;
    let dw = source_family_weights(dev)?;
    let cw = source_family_weights(cal)?;
    let dimensions = width(contract, mode);
    let mut prior = [0.0_f64; 3];
    for (r, w) in train.iter().zip(&tw) {
        prior[r.label.unwrap().index()] += w;
    }
    let mut head = Head {
        mode,
        weights: std::array::from_fn(|_| vec![0.0; dimensions]),
        bias: prior.map(f64::ln),
        temperature: 1.0,
        selected_epoch: 0,
    };
    let mut first = std::array::from_fn::<_, 3, _>(|_| vec![0.0; dimensions + 1]);
    let mut second = first.clone();
    let mut best = head.clone();
    let mut best_loss = loss(&head, &dev_x, dev, &dw, 1.0)?;
    let mut history = vec![Epoch {
        epoch: 0,
        train_family_log_loss: loss(&head, &train_x, train, &tw, 1.0)?,
        development_family_log_loss: best_loss,
    }];
    for epoch in 1..=config.epochs {
        let mut gradient = std::array::from_fn::<_, 3, _>(|_| vec![0.0; dimensions + 1]);
        for ((row, x), w) in train.iter().zip(&train_x).zip(&tw) {
            let p = probabilities(&head, x, 1.0)?;
            for c in 0..3 {
                let residual = w
                    * (p[c]
                        - if row.label.unwrap().index() == c {
                            1.0
                        } else {
                            0.0
                        });
                gradient[c][dimensions] += residual;
                for (i, v) in x.iter().enumerate() {
                    gradient[c][i] += residual * v;
                }
            }
        }
        let correction1 = 1.0 - 0.9_f64.powf(epoch as f64);
        let correction2 = 1.0 - 0.999_f64.powf(epoch as f64);
        for c in 0..3 {
            for i in 0..=dimensions {
                let coefficient = if i == dimensions {
                    &mut head.bias[c]
                } else {
                    &mut head.weights[c][i]
                };
                let derivative = gradient[c][i]
                    + if i < dimensions {
                        config.l2 * *coefficient
                    } else {
                        0.0
                    };
                first[c][i] = 0.9 * first[c][i] + 0.1 * derivative;
                second[c][i] = 0.999 * second[c][i] + 0.001 * derivative * derivative;
                *coefficient -= config.learning_rate * (first[c][i] / correction1)
                    / ((second[c][i] / correction2).sqrt() + 1e-8);
            }
        }
        let dev_loss = loss(&head, &dev_x, dev, &dw, 1.0)?;
        history.push(Epoch {
            epoch,
            train_family_log_loss: loss(&head, &train_x, train, &tw, 1.0)?,
            development_family_log_loss: dev_loss,
        });
        if dev_loss < best_loss - 1e-9 {
            best_loss = dev_loss;
            best = head.clone();
            best.selected_epoch = epoch;
        }
        if epoch - best.selected_epoch >= config.patience {
            break;
        }
    }
    // Calibration does not choose weights, epochs, architecture or feature scales.
    let mut cal_loss = loss(&best, &cal_x, cal, &cw, 1.0)?;
    for step in -40..=40 {
        let temperature = (step as f64 * 0.1).exp();
        let candidate = loss(&best, &cal_x, cal, &cw, temperature)?;
        if candidate < cal_loss - 1e-12 {
            cal_loss = candidate;
            best.temperature = temperature;
        }
    }
    let report = HeadReport {
        mode,
        parameters: 3 * (dimensions + 1),
        selected_epoch: best.selected_epoch,
        temperature: best.temperature,
        history,
        development_uncalibrated: metrics(&best, &dev_x, dev, 1.0, config.abstain_below)?,
        calibration_uncalibrated: metrics(&best, &cal_x, cal, 1.0, config.abstain_below)?,
        calibration_fitted: metrics(&best, &cal_x, cal, best.temperature, config.abstain_below)?,
    };
    Ok((best, report))
}

pub fn implementation_identity() -> Result<StableId> {
    Ok(StableId(digest(
        "slop_ninja_a_baseline_implementation_v1",
        &(
            include_str!("lib.rs"),
            include_str!("contract.rs"),
            include_str!("model.rs"),
            include_str!("../Cargo.toml"),
        ),
    )?))
}

pub fn fit(
    contract: &Contract,
    rows: &[NumericRow],
    audit: &ExportAudit,
    config: &TrainConfig,
) -> Result<FitOutcome> {
    validate_export(contract, rows, audit)?;
    config.validate()?;
    ensure!(
        rows.iter().all(|r| r.split != Split::Test),
        "Test rows may not enter fit/selection/calibration"
    );
    let summaries: Vec<_> = [Split::Train, Split::Development, Split::Calibration]
        .map(|s| partition_summary(contract, rows, s))
        .into();
    if summaries.iter().any(|s| {
        s.groups < config.min_groups_per_split
            || s.class_rows.iter().any(|n| *n < config.min_rows_per_class)
    }) {
        return Ok(FitOutcome::Unavailable{reason:"identical eligible rows must contain adequate source groups and all three origin labels in Train, Development and Calibration".into(),partitions:summaries});
    }
    let mut included: Vec<_> = rows.iter().filter(|r| eligible(contract, r)).collect();
    included.sort_by(|a, b| a.id.cmp(&b.id));
    let partition = |s| {
        included
            .iter()
            .copied()
            .filter(|r| r.split == s)
            .collect::<Vec<_>>()
    };
    let train = partition(Split::Train);
    let dev = partition(Split::Development);
    let cal = partition(Split::Calibration);
    if !train
        .iter()
        .any(|r| r.input.lexical.available.iter().any(|v| *v))
        || !train
            .iter()
            .any(|r| r.input.grammar.available.iter().any(|v| *v))
    {
        return Ok(FitOutcome::Unavailable{reason:"no actual measured lexical or grammar features in eligible Train rows; availability alone is not a grammar comparison".into(),partitions:summaries});
    }
    let scales = fit_scales(
        contract,
        &train,
        &source_family_weights(&train)?,
        config.scale_floor,
    )?;
    let mut heads = Vec::new();
    let mut reports = Vec::new();
    for mode in MODES {
        let (head, report) = fit_head(contract, &scales, &train, &dev, &cal, config, mode)?;
        heads.push(head);
        reports.push(report);
    }
    let mut bindings: Vec<_> = rows.iter().map(RowBinding::from).collect();
    bindings.sort_by(|a, b| a.id.cmp(&b.id));
    let eligible_rows: Vec<_> = included.iter().map(|r| r.id.clone()).collect();
    let content = ModelContent {
        schema: MODEL_SCHEMA.into(),
        contract: contract.clone(),
        contract_sha256: contract.identity()?,
        implementation_sha256: implementation_identity()?,
        export_audit_sha256: StableId(digest(AUDIT_SCHEMA, audit)?),
        training_rows_sha256: rows_identity(rows)?,
        eligible_rows_sha256: StableId(digest("slop_ninja_a_eligible_rows_v1", &eligible_rows)?),
        classes: CLASS_NAMES.map(str::to_owned),
        config: config.clone(),
        scales,
        heads: heads
            .try_into()
            .map_err(|_| anyhow::anyhow!("missing comparison head"))?,
        fitting_bindings: bindings,
    };
    let artifact = Artifact {
        sha256: StableId(digest(MODEL_SCHEMA, &content)?),
        content,
    };
    artifact.validate()?;
    Ok(FitOutcome::Ready {
        artifact: Box::new(artifact),
        report: ComparisonReport {
            schema: REPORT_SCHEMA.into(),
            grammar_evidence: contract.grammar_evidence,
            partitions: summaries,
            heads: reports,
            semantic_admission: false,
        },
    })
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct HeadPrediction {
    pub mode: Mode,
    pub probabilities: [f64; 3],
    pub abstained: bool,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
pub enum Prediction {
    Available { heads: [HeadPrediction; 3] },
    Unavailable { reason: String },
}

/// Validate the artifact once before repeatedly calling predict_validated in a batch.
pub fn predict(artifact: &Artifact, input: &NumericInput) -> Result<Prediction> {
    artifact.validate()?;
    predict_validated(artifact, input)
}
pub fn predict_validated(artifact: &Artifact, input: &NumericInput) -> Result<Prediction> {
    let c = &artifact.content;
    input.validate(&c.contract)?;
    if c.contract.row_policy == RowPolicy::CompletePairedDocuments && !input.complete() {
        return Ok(Prediction::Unavailable {
            reason: "incomplete paired document under frozen eligibility policy".into(),
        });
    }
    let mut heads = Vec::new();
    for head in &c.heads {
        let x = features(&c.contract, &c.scales, input, head.mode)?;
        let p = probabilities(head, &x, head.temperature)?;
        heads.push(HeadPrediction {
            mode: head.mode,
            probabilities: p,
            abstained: c.config.abstain_below.is_some_and(|a| p[argmax(p)] < a),
        });
    }
    Ok(Prediction::Available {
        heads: heads
            .try_into()
            .map_err(|_| anyhow::anyhow!("missing prediction head"))?,
    })
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Evaluation {
    pub artifact_sha256: StableId,
    pub export_audit_sha256: StableId,
    pub partition: PartitionSummary,
    pub metrics: Vec<(Mode, Metrics)>,
    pub available: bool,
    pub semantic_admission: bool,
}

/// Final Test evaluation never fits any parameter, threshold, scale or temperature.
pub fn evaluate(
    artifact: &Artifact,
    rows: &[NumericRow],
    audit: &ExportAudit,
) -> Result<Evaluation> {
    artifact.validate()?;
    let c = &artifact.content;
    validate_export(&c.contract, rows, audit)?;
    ensure!(
        rows.iter().all(|r| r.split == Split::Test),
        "evaluation accepts Test rows only"
    );
    let mut groups = BTreeSet::new();
    let mut texts = BTreeSet::new();
    let mut normalized = BTreeSet::new();
    let mut lineage = BTreeSet::new();
    for b in &c.fitting_bindings {
        groups.insert(&b.source_group);
        texts.insert(&b.text_sha256);
        normalized.insert(&b.normalized_text_sha256);
        lineage.insert(&b.id);
        lineage.insert(&b.lineage_root);
        lineage.extend(&b.ancestor_ids);
    }
    for r in rows {
        ensure!(
            !groups.contains(&r.source_group)
                && !texts.contains(&r.text_sha256)
                && !normalized.contains(&r.normalized_text_sha256),
            "Test overlaps fitting/selection/calibration ledger"
        );
        ensure!(
            std::iter::once(&r.id)
                .chain(std::iter::once(&r.lineage_root))
                .chain(r.ancestor_ids.iter())
                .all(|id| !lineage.contains(id)),
            "Test ancestry overlaps fitting/selection/calibration ledger"
        );
    }
    let summary = partition_summary(&c.contract, rows, Split::Test);
    let mut included: Vec<_> = rows.iter().filter(|r| eligible(&c.contract, r)).collect();
    included.sort_by(|a, b| a.id.cmp(&b.id));
    let mut reports = Vec::new();
    if !included.is_empty() {
        for head in &c.heads {
            let x = included
                .iter()
                .map(|r| features(&c.contract, &c.scales, &r.input, head.mode))
                .collect::<Result<Vec<_>>>()?;
            reports.push((
                head.mode,
                metrics(
                    head,
                    &x,
                    &included,
                    head.temperature,
                    c.config.abstain_below,
                )?,
            ));
        }
    }
    Ok(Evaluation {
        artifact_sha256: artifact.sha256.clone(),
        export_audit_sha256: StableId(digest(AUDIT_SCHEMA, audit)?),
        partition: summary,
        metrics: reports,
        available: !included.is_empty(),
        semantic_admission: false,
    })
}
