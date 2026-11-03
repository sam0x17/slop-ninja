//! Bundle weights.
//!
//! **Train (`domain_origin_share_v1`).** The loss mass of the whole split is
//! divided in four steps, and `weights.json` reports the mass that results
//! per origin, domain, cohort and label:
//!
//! 1. **Domains.** Domain `d` gets mass proportional to its number of source
//!    groups among rows with positive cohort weight (every group counts once,
//!    whatever origins it holds).
//! 2. **Origin cells.** Within a domain the human, mixed and model-only cells
//!    carry the declared shares (`weighting.human_share`, default 1/3; the
//!    remainder split equally between mixed and model-only unless declared).
//!    Groups are pooled: a cell is the set of all rows of that domain and
//!    origin, so a group need not contain every origin. A cell with no rows
//!    hands its share to the domain's other cells in proportion.
//! 3. **Inside a cell.** Every source group of the cell gets equal mass and
//!    every document of a group in the cell equal mass (family balance); each
//!    row is then multiplied by its declared cohort weight
//!    (`cohort_weights`), and the cell is renormalised to its mass, so cohort
//!    weights move mass between cohorts of one cell, never between cells.
//!    A cohort weight of 0 removes the cohort.
//! 4. **Frontier label balance** (`hosted_frontier_label_balanced_v1`), last.
//!    In every hosted cohort (frontier records and hosted captures) each
//!    attribution label gets the same total weight (or the declared relative
//!    label weight), holding the cohort's total. Iterative proportional
//!    fitting alternates the cell masses of step 2 and the label totals; it
//!    ends on the label step, so the label totals hold exactly on the final
//!    weights and the cell masses to the reported deviation.
//!
//! The weights are then scaled so their mean over rows with positive weight
//! is 1. `balanced_weight` is the step-3 family-balanced weight before cohort
//! factors, covariate raking and label balance (same scale), `cohort_weight`
//! the declared cohort factor and `weight` the final weight.
//!
//! **Evaluation splits.** `weight = balanced_weight` within each population:
//! equal source-group mass, equal observed origin-cell mass within a group,
//! equal document mass within a cell, scaled to mean 1; cohort weights are
//! not applied.
//!
//! **Round 2** (protocol A2 §6.3–§6.4 and amendment 5; only with a `round2`
//! config block that turns an option on, so a config without one keeps its
//! exact bytes): `a_covariate_balance_v1` rakes the AI-involved cells between
//! steps 3 and 4 (`covariate`); the Train scheme is `TRAIN_SCHEME_R2` and the
//! Train `weights.json` gains a `round2` report with the Kish ESS under the
//! trainer's weighting (a bundle below R0's floor is refused), the human
//! share of document-loss mass, the RAID share of AI-involved mass before and
//! under the trainer recipe's inherited caps, the masses of the declared
//! round-2 cohort roles and, with block tags, the mass per block. A
//! self-weighting role (always `cc_n2`) refuses any declared cohort factor
//! and any declared label factor of its cohorts, hosted cohorts included,
//! other than 1; a role with a writer-key allow-list refuses rows outside it.
use crate::bundle::{CohortRole, Round2};
use crate::covariate::{self, COVARIATE_RULE, R0_RAID_CAP, R0_RAID_PREFIX};
use crate::{FRONTIER_POLICY, ORIGINS, Row, SourceKind};
use anyhow::{Result, bail, ensure};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet, HashSet};

pub const TRAIN_SCHEME: &str = "domain_origin_share_v1";
/// The Train scheme of a round-2 build: v1 with the round-2 options (raking
/// between steps 3 and 4 when covariate balance is on; amendment 5 item 5.4).
pub const TRAIN_SCHEME_R2: &str = "domain_origin_share_r2_v1";

/// Declared cohort weights: a default plus per-cohort factors; per-label
/// factors inside a hosted cohort are relative label targets of step 4, and
/// inside any other cohort multiply the label's rows.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct CohortWeights {
    #[serde(default = "one")]
    pub default: f64,
    #[serde(default)]
    pub cohorts: BTreeMap<String, f64>,
    #[serde(default)]
    pub labels: BTreeMap<String, BTreeMap<String, f64>>,
}

fn one() -> f64 {
    1.0
}

/// A config without `cohort_weights` weights every cohort 1 (not 0).
impl Default for CohortWeights {
    fn default() -> Self {
        CohortWeights { default: 1.0, cohorts: BTreeMap::new(), labels: BTreeMap::new() }
    }
}

/// Declared Train origin shares and the label-balance fit controls.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Weighting {
    #[serde(default = "third")]
    pub human_share: f64,
    #[serde(default)]
    pub mixed_share: Option<f64>,
    #[serde(default)]
    pub model_only_share: Option<f64>,
    #[serde(default = "iters")]
    pub max_label_fit_iterations: usize,
    #[serde(default = "tol")]
    pub label_fit_tolerance: f64,
}

fn third() -> f64 {
    1.0 / 3.0
}
fn iters() -> usize {
    5000
}
fn tol() -> f64 {
    1e-10
}

impl Default for Weighting {
    fn default() -> Self {
        Weighting { human_share: third(), mixed_share: None, model_only_share: None,
            max_label_fit_iterations: iters(), label_fit_tolerance: tol() }
    }
}

impl Weighting {
    /// Shares in `ORIGINS` order (human_only, mixed, model_only).
    pub fn shares(&self) -> Result<[f64; 3]> {
        let h = self.human_share;
        let m = self.mixed_share.unwrap_or_else(|| self.model_only_share.map_or((1.0 - h) / 2.0, |o| 1.0 - h - o));
        let o = self.model_only_share.unwrap_or(1.0 - h - m);
        for x in [h, m, o] {
            ensure!(x.is_finite() && (0.0..=1.0).contains(&x), "origin shares must lie in [0,1]");
        }
        ensure!((h + m + o - 1.0).abs() < 1e-9, "origin shares must sum to 1");
        Ok([h, m, o])
    }
}

/// Family/origin-balanced weights (equal source-group mass, equal origin-cell
/// mass within a group, equal document mass within a cell), scaled to mean 1.
pub fn balanced_weights(rows: &[&Row]) -> Vec<f64> {
    let mut cells: BTreeMap<&str, BTreeMap<&str, usize>> = BTreeMap::new();
    for r in rows {
        *cells.entry(&r.source_group).or_default().entry(&r.origin).or_default() += 1;
    }
    let g = cells.len() as f64;
    let n = rows.len() as f64;
    rows.iter()
        .map(|r| {
            let c = &cells[r.source_group.as_str()];
            n / (g * c.len() as f64 * c[r.origin.as_str()] as f64)
        })
        .collect()
}

fn label(r: &Row) -> String {
    r.generator_label.clone().unwrap_or_else(|| "human".into())
}

/// Hosted cohorts take the frontier label balance.
fn hosted(r: &Row) -> bool {
    matches!(r.source_kind, SourceKind::Frontier | SourceKind::HostedCapture)
}

fn origin_index(o: &str) -> usize {
    ORIGINS.iter().position(|x| *x == o).expect("origin checked on read")
}

/// The declared factor of a row: cohort weight, times a label factor outside hosted cohorts.
fn declared_factor(r: &Row, cw: &CohortWeights) -> f64 {
    let base = cw.cohorts.get(&r.cohort_id).copied().unwrap_or(cw.default);
    let lab = cw.labels.get(&r.cohort_id).and_then(|m| m.get(&label(r))).copied();
    match lab {
        Some(f) if f == 0.0 => 0.0,
        Some(f) if !hosted(r) => base * f,
        _ => base,
    }
}

type Mass = BTreeMap<String, (usize, f64)>;

fn mass_json(m: &Mass, total: f64) -> Value {
    m.iter()
        .map(|(k, (n, w))| (k.clone(), json!({"rows": n, "weight_sum": w, "mass_share": if total > 0.0 { w / total } else { 0.0 }})))
        .collect::<serde_json::Map<_, _>>()
        .into()
}

/// Weights of one split; returns the weights.json body.
pub fn assign_weights(rows: &mut [Row], train: bool, cw: &CohortWeights, wt: &Weighting) -> Result<Value> {
    assign_weights_with(rows, train, cw, wt, None)
}

/// Round-2 Train options and the writer-key allow-lists of their cohort roles
/// (in `opts.cohort_roles` order; `None` where a role has none).
pub struct Round2Train<'a> {
    pub opts: &'a Round2,
    pub writer_keys: &'a [Option<HashSet<String>>],
}

/// `assign_weights` with the round-2 Train options (ignored for evaluation splits).
pub fn assign_weights_with(rows: &mut [Row], train: bool, cw: &CohortWeights, wt: &Weighting, r2: Option<&Round2Train>) -> Result<Value> {
    if !train {
        return Ok(evaluation_weights(rows));
    }
    let shares = wt.shares()?;
    let n = rows.len();
    let factor: Vec<f64> = rows.iter().map(|r| declared_factor(r, cw)).collect();
    for f in &factor {
        ensure!(f.is_finite() && *f >= 0.0, "cohort weights must be finite and >= 0");
    }
    let live: Vec<usize> = (0..n).filter(|&i| factor[i] > 0.0).collect();
    // Step 1: domain mass by source groups.
    let mut dom_groups: BTreeMap<&str, BTreeSet<&str>> = BTreeMap::new();
    for &i in &live {
        dom_groups.entry(&rows[i].domain).or_default().insert(&rows[i].source_group);
    }
    let all_groups: usize = dom_groups.values().map(BTreeSet::len).sum();
    // Step 2: cell masses.
    let mut cell_rows: BTreeMap<(String, usize), Vec<usize>> = BTreeMap::new();
    for &i in &live {
        cell_rows.entry((rows[i].domain.clone(), origin_index(&rows[i].origin))).or_default().push(i);
    }
    let mut cell_target: BTreeMap<(String, usize), f64> = BTreeMap::new();
    for (d, gs) in &dom_groups {
        let present: Vec<usize> = (0..3).filter(|o| cell_rows.contains_key(&(d.to_string(), *o))).collect();
        let s: f64 = present.iter().map(|&o| shares[o]).sum();
        let dm = gs.len() as f64 / all_groups.max(1) as f64;
        for &o in &present {
            // A domain whose present cells all have declared share 0 splits its mass equally.
            let t = if s > 0.0 { dm * shares[o] / s } else { dm / present.len() as f64 };
            cell_target.insert((d.to_string(), o), t);
        }
    }
    // Step 3: family balance and cohort factors inside each cell.
    let mut w = vec![0.0f64; n];
    let mut bal = vec![0.0f64; n];
    for (key, idx) in &cell_rows {
        let mut per_group: BTreeMap<&str, usize> = BTreeMap::new();
        for &i in idx {
            *per_group.entry(&rows[i].source_group).or_default() += 1;
        }
        let g = per_group.len() as f64;
        let fb: Vec<f64> = idx.iter().map(|&i| 1.0 / (g * per_group[rows[i].source_group.as_str()] as f64)).collect();
        let t = cell_target[key];
        let su: f64 = idx.iter().zip(&fb).map(|(&i, b)| b * factor[i]).sum();
        for (&i, b) in idx.iter().zip(&fb) {
            bal[i] = t * b; // fb sums to 1 over the cell
            w[i] = t * b * factor[i] / su;
        }
    }
    // Round 2: covariate balance of the AI-involved cells, after step 3 and before the label step.
    let raked = match r2.and_then(|x| x.opts.covariate_balance.as_deref()) {
        Some(rule) => {
            ensure!(rule == COVARIATE_RULE, "unknown covariate balance rule {rule} (known: {COVARIATE_RULE})");
            Some(covariate::rake(rows, &mut w)?)
        }
        None => None,
    };
    // Step 4: label balance in hosted cohorts (iterative proportional fitting, label step last).
    let mut col: Vec<Option<(String, String)>> = vec![None; n];
    let mut col_target: BTreeMap<(String, String), f64> = BTreeMap::new();
    let mut cohort_mass: BTreeMap<String, f64> = BTreeMap::new();
    for &i in &live {
        if hosted(&rows[i]) {
            *cohort_mass.entry(rows[i].cohort_id.clone()).or_default() += w[i];
            col[i] = Some((rows[i].cohort_id.clone(), label(&rows[i])));
        }
    }
    for (c, m) in &cohort_mass {
        let labels: BTreeSet<String> = live.iter().filter(|&&i| col[i].as_ref().is_some_and(|k| &k.0 == c)).map(|&i| label(&rows[i])).collect();
        let lam: Vec<(String, f64)> = labels.iter().map(|l| (l.clone(), cw.labels.get(c).and_then(|x| x.get(l)).copied().unwrap_or(1.0))).collect();
        let s: f64 = lam.iter().map(|x| x.1).sum();
        for (l, x) in lam {
            col_target.insert((c.clone(), l), m * x / s);
        }
    }
    let mut iterations = 0usize;
    let mut cell_dev = 0.0f64;
    if !col_target.is_empty() {
        let rest_target = 1.0 - cohort_mass.values().sum::<f64>();
        loop {
            iterations += 1;
            // cell step
            for (key, idx) in &cell_rows {
                let s: f64 = idx.iter().map(|&i| w[i]).sum();
                if s > 0.0 {
                    let f = cell_target[key] / s;
                    for &i in idx {
                        w[i] *= f;
                    }
                }
            }
            // label step (the remaining rows form one column holding their mass)
            let mut sums: BTreeMap<Option<&(String, String)>, f64> = BTreeMap::new();
            for &i in &live {
                *sums.entry(col[i].as_ref()).or_default() += w[i];
            }
            for &i in &live {
                let s = sums[&col[i].as_ref()];
                let t = match &col[i] { Some(k) => col_target[k], None => rest_target };
                if s > 0.0 {
                    w[i] *= t / s;
                }
            }
            cell_dev = cell_rows.iter().map(|(key, idx)| {
                let s: f64 = idx.iter().map(|&i| w[i]).sum();
                ((s - cell_target[key]) / cell_target[key]).abs()
            }).fold(0.0, f64::max);
            if cell_dev < wt.label_fit_tolerance || iterations >= wt.max_label_fit_iterations {
                break;
            }
        }
    }
    // Scale to mean 1 over rows with positive weight.
    let total: f64 = w.iter().sum();
    let nz = w.iter().filter(|x| **x > 0.0).count();
    let norm = if total > 0.0 { nz as f64 / total } else { 0.0 };
    for i in 0..n {
        rows[i].cohort_weight = factor[i];
        rows[i].balanced_weight = bal[i] * norm;
        rows[i].weight = w[i] * norm;
    }
    let mut report = train_report(rows, cw, wt, &shares, &cell_target, iterations, cell_dev, &col_target, norm);
    if let Some(x) = r2 {
        let final_w: Vec<f64> = rows.iter().map(|r| r.weight).collect();
        let tw = covariate::trainer_weighting(&rows.iter().map(|r| (origin_index(&r.origin), r.cohort_id.as_str(), r.weight)).collect::<Vec<_>>())?;
        let cov = raked.map(|r| r.finish(rows, &final_w, &tw.w));
        report["scheme"] = json!(TRAIN_SCHEME_R2);
        report["round2"] = round2_report(rows, cw, x, cov, &tw, &report)?;
    }
    Ok(report)
}

/// Round-2 cohort roles (protocol A2 §6.1, §6.4).
pub const ROLE_ENRON: &str = "enron_train_half";
pub const ROLE_BASE: &str = "base_model";
pub const ROLE_CC: &str = "cc_n2";
pub const ROLES: [&str; 3] = [ROLE_ENRON, ROLE_BASE, ROLE_CC];

impl CohortRole {
    pub fn matches(&self, r: &Row) -> bool {
        self.cohort_prefixes.iter().any(|p| r.cohort_id.starts_with(p.as_str()))
            || r.collection.as_ref().is_some_and(|c| self.collections.contains(c))
    }
    /// `cc_n2` is self-weighting by protocol; others when declared.
    pub fn self_weighting(&self) -> bool {
        self.self_weighting || self.role == ROLE_CC
    }
}

/// Check the round-2 roles' declarations against `opts` alone (before any row is read).
pub fn check_roles(opts: &Round2) -> Result<()> {
    let mut seen = BTreeSet::new();
    for c in &opts.cohort_roles {
        ensure!(ROLES.contains(&c.role.as_str()), "unknown round-2 cohort role {} (known: {ROLES:?})", c.role);
        ensure!(seen.insert(c.role.as_str()), "round-2 cohort role {} declared twice", c.role);
        ensure!(!c.cohort_prefixes.is_empty() || !c.collections.is_empty(), "round-2 cohort role {} selects nothing", c.role);
        ensure!(c.cohort_prefixes.iter().all(|p| !p.is_empty()), "round-2 cohort role {}: empty cohort prefix", c.role);
        for p in &c.cohort_prefixes {
            ensure!(!p.starts_with(R0_RAID_PREFIX) && !R0_RAID_PREFIX.starts_with(p.as_str()),
                "round-2 cohort role {}: prefix {p} overlaps the inherited RAID cap prefix {R0_RAID_PREFIX}", c.role);
        }
        if c.role == ROLE_ENRON {
            ensure!(c.collections.iter().any(|x| x == grammar_a_pools::pile::ENRON_COLLECTION),
                "{ROLE_ENRON} must select the collection {}", grammar_a_pools::pile::ENRON_COLLECTION);
            ensure!(c.writer_keys.is_some(), "{ROLE_ENRON} needs its writer-key allow-list (S0-9 Train half, V23)");
        }
    }
    Ok(())
}

fn round2_report(rows: &[Row], cw: &CohortWeights, r2: &Round2Train, covariate: Option<Value>, tw: &covariate::TrainerWeights,
    base: &Value) -> Result<Value> {
    let opts = r2.opts;
    check_roles(opts)?;
    ensure!(r2.writer_keys.len() == opts.cohort_roles.len(), "writer-key lists do not match the cohort roles");
    let total: f64 = rows.iter().map(|r| r.weight).sum();
    let ai_total: f64 = rows.iter().filter(|r| r.origin != "human_only").map(|r| r.weight).sum();
    // Roles: exclusive membership, self-weighting and writer keys.
    let mut role_of: Vec<Option<usize>> = vec![None; rows.len()];
    for (i, r) in rows.iter().enumerate() {
        for (k, c) in opts.cohort_roles.iter().enumerate() {
            if c.matches(r) {
                if let Some(j) = role_of[i] {
                    bail!("row {} matches round-2 cohort roles {} and {}", r.id, opts.cohort_roles[j].role, c.role);
                }
                role_of[i] = Some(k);
            }
        }
        if role_of[i].is_none() && r.collection.as_deref() == Some(grammar_a_pools::pile::ENRON_COLLECTION) {
            bail!("Train row {} is an {} row outside a declared {ROLE_ENRON} role", r.id, grammar_a_pools::pile::ENRON_COLLECTION);
        }
        if r.cohort_id.starts_with(R0_RAID_PREFIX) && role_of[i].is_some() {
            bail!("Train row {} of a round-2 cohort role falls under the inherited RAID cap prefix {R0_RAID_PREFIX}", r.id);
        }
    }
    let mut roles = serde_json::Map::new();
    for (k, c) in opts.cohort_roles.iter().enumerate() {
        let idx: Vec<usize> = (0..rows.len()).filter(|&i| role_of[i] == Some(k)).collect();
        let mut by_origin: BTreeMap<&str, (usize, f64)> = BTreeMap::new();
        // cohort -> (rows, weight sum, smallest and largest declared factor)
        let mut cohorts: BTreeMap<&str, (usize, f64, f64, f64)> = BTreeMap::new();
        for &i in &idx {
            let r = &rows[i];
            let f = declared_factor(r, cw);
            if c.self_weighting() {
                ensure!(f == 1.0, "row {} of self-weighting role {} has declared factor {f} (protocol A2 §6.4: no cohort multiplier)", r.id, c.role);
                // Hosted cohorts take label factors as relative label targets (step 4), outside `declared_factor`.
                if let Some((l, x)) = cw.labels.get(&r.cohort_id).and_then(|m| m.iter().find(|(_, x)| **x != 1.0)) {
                    bail!("cohort {} of self-weighting role {} declares label factor {x} for {l} (protocol A2 §6.4: no cohort multiplier)", r.cohort_id, c.role);
                }
            }
            if let Some(keys) = &r2.writer_keys[k] {
                let w = r.writer_key.as_deref().unwrap_or("");
                ensure!(keys.contains(w), "row {} of role {} has a writer key outside its allow-list", r.id, c.role);
            }
            let e = by_origin.entry(r.origin.as_str()).or_default();
            e.0 += 1;
            e.1 += r.weight;
            let e = cohorts.entry(r.cohort_id.as_str()).or_insert((0, 0.0, f, f));
            e.0 += 1;
            e.1 += r.weight;
            e.2 = e.2.min(f);
            e.3 = e.3.max(f);
        }
        let ws: f64 = idx.iter().map(|&i| rows[i].weight).sum();
        roles.insert(c.role.clone(), json!({
            "cohort_prefixes": c.cohort_prefixes, "collections": c.collections, "self_weighting": c.self_weighting(),
            "writer_key_allow_list": r2.writer_keys[k].as_ref().map(|s| json!({"keys": s.len(), "rows_checked": idx.len()})),
            "rows": idx.len(), "weight_sum": ws, "mass_share": if total > 0.0 { ws / total } else { 0.0 },
            "by_origin": by_origin.iter().map(|(o, (n, w))| (o.to_string(), json!({"rows": n, "weight_sum": w}))).collect::<serde_json::Map<_, _>>(),
            "cohorts": cohorts.iter().map(|(id, (n, w, lo, hi))| (id.to_string(),
                json!({"rows": n, "weight_sum": w, "declared_factor_min": lo, "declared_factor_max": hi}))).collect::<serde_json::Map<_, _>>(),
        }));
    }
    let raid: f64 = rows.iter().filter(|r| r.origin != "human_only" && r.cohort_id.starts_with(R0_RAID_PREFIX)).map(|r| r.weight).sum();
    let raid_share = if ai_total > 0.0 { raid / ai_total } else { 0.0 };
    let blocks = opts.block_tags.as_ref().map(|_| {
        let mut m: BTreeMap<String, (usize, f64)> = BTreeMap::new();
        for r in rows {
            let e = m.entry(r.block.clone().unwrap_or_default()).or_default();
            e.0 += 1;
            e.1 += r.weight;
        }
        m.into_iter().map(|(b, (n, w))| (b, json!({"rows": n, "weight_sum": w, "mass_share": if total > 0.0 { w / total } else { 0.0 }})))
            .collect::<serde_json::Map<_, _>>()
    });
    Ok(json!({
        "schema": "slop_ninja_a_r2_train_weights_v1",
        "kish_ess": covariate::ess_check(tw)?,
        "human_share_of_document_loss_mass": base["effective_mass"]["by_origin"]["human_only"]["mass_share"].clone(),
        "raid": {"prefix": R0_RAID_PREFIX, "trainer_cap": R0_RAID_CAP, "ai_involved_mass_share_before_trainer_caps": raid_share,
            "trainer_cap_binds": raid_share > R0_RAID_CAP, "ai_involved_mass_share_trainer_weighting": tw.raid_share_of_ai_involved,
            "rule": "inherited: the trainer recipe's cohort_positive_mass_caps holds the RAID cohorts at 20% of AI-involved mass after the row caps; the assembler reports the share before it and under the trainer's weighting"},
        "covariate_balance": covariate,
        "cohort_roles": roles,
        "blocks": blocks,
    }))
}

#[allow(clippy::too_many_arguments)]
fn train_report(rows: &[Row], cw: &CohortWeights, wt: &Weighting, shares: &[f64; 3], cell_target: &BTreeMap<(String, usize), f64>,
    iterations: usize, cell_dev: f64, col_target: &BTreeMap<(String, String), f64>, norm: f64) -> Value {
    let total: f64 = rows.iter().map(|r| r.weight).sum();
    let mut by_origin = Mass::new();
    let mut by_cohort = Mass::new();
    let mut by_label = Mass::new();
    let mut by_cohort_label: BTreeMap<String, Mass> = BTreeMap::new();
    let mut by_domain_origin: BTreeMap<String, Mass> = BTreeMap::new();
    let mut by_kind = Mass::new();
    for r in rows {
        for (m, k) in [(&mut by_origin, r.origin.clone()), (&mut by_cohort, r.cohort_id.clone()), (&mut by_label, label(r)),
            (&mut by_kind, format!("{:?}", r.source_kind))] {
            let e = m.entry(k).or_default();
            e.0 += 1;
            e.1 += r.weight;
        }
        let e = by_cohort_label.entry(r.cohort_id.clone()).or_default().entry(label(r)).or_default();
        e.0 += 1;
        e.1 += r.weight;
        let e = by_domain_origin.entry(r.domain.clone()).or_default().entry(r.origin.clone()).or_default();
        e.0 += 1;
        e.1 += r.weight;
    }
    // Label equality on the final weights, per hosted cohort.
    let mut label_check = serde_json::Map::new();
    let cohorts: BTreeSet<&String> = col_target.keys().map(|k| &k.0).collect();
    for c in cohorts {
        let m = &by_cohort_label[c];
        let cohort_total: f64 = m.values().map(|x| x.1).sum();
        let dev = col_target.iter().filter(|(k, _)| &k.0 == c)
            .map(|(k, t)| (m[&k.1].1 / (t * norm) - 1.0).abs()).fold(0.0, f64::max);
        label_check.insert(c.clone(), json!({"labels": m.len(), "cohort_weight_sum": cohort_total,
            "max_relative_deviation_from_label_target": dev,
            "label_targets_relative": col_target.iter().filter(|(k, _)| &k.0 == c).map(|(k, t)| (k.1.clone(), json!(t * norm / cohort_total))).collect::<serde_json::Map<_, _>>()}));
    }
    let cells: Vec<Value> = cell_target.iter().map(|((d, o), t)| {
        let got = by_domain_origin.get(d).and_then(|m| m.get(ORIGINS[*o])).map_or(0.0, |x| x.1);
        json!({"domain": d, "origin": ORIGINS[*o], "target_mass_share": t, "mass_share": got / total})
    }).collect();
    let mut cohorts = serde_json::Map::new();
    for (c, m) in &by_cohort_label {
        let h = rows.iter().any(|r| &r.cohort_id == c && hosted(r));
        let declared = cw.cohorts.get(c).copied();
        cohorts.insert(c.clone(), json!({"rows": m.values().map(|x| x.0).sum::<usize>(),
            "cohort_weight": declared.unwrap_or(cw.default), "default": declared.is_none(),
            "policy": if h { FRONTIER_POLICY } else { "uniform" },
            "weight_sum": m.values().map(|x| x.1).sum::<f64>(), "labels": mass_json(m, total)}));
    }
    json!({
        "schema": "slop_ninja_a_bundle_weights_v2", "train": true, "scheme": TRAIN_SCHEME,
        "rule": "domain mass by source groups; within a domain the human, mixed and model-only cells carry the declared shares over pooled groups (absent cells hand their share to the present ones); inside a cell equal source-group and document mass times the declared cohort weight, renormalised to the cell; last, every hosted cohort gives each attribution label the same total weight (iterative proportional fitting ending on the label step); scaled to mean 1 over rows with positive weight",
        "declared": {"cohort_weights": cw, "weighting": wt, "origin_shares": {"human_only": shares[0], "mixed": shares[1], "model_only": shares[2]}},
        "normalisation": norm, "total_weight": total,
        "label_fit": {"iterations": iterations, "max_relative_cell_deviation": cell_dev, "cohorts": label_check},
        "effective_mass": {"by_origin": mass_json(&by_origin, total), "by_source_kind": mass_json(&by_kind, total),
            "by_cohort": mass_json(&by_cohort, total), "by_label": mass_json(&by_label, total),
            "by_domain_origin": by_domain_origin.iter().map(|(d, m)| (d.clone(), mass_json(m, total))).collect::<serde_json::Map<_, _>>()},
        "cells": cells, "cohorts": cohorts,
    })
}

/// Frontier rows belong to the primary population (protocol `primary_metric`):
/// their balanced weights are computed together with the primary rows.
pub fn weight_pool(population: &str) -> &str {
    if population == "frontier" { "primary" } else { population }
}

fn evaluation_weights(rows: &mut [Row]) -> Value {
    let mut by_pop: BTreeMap<String, Vec<usize>> = BTreeMap::new();
    for (i, r) in rows.iter().enumerate() {
        by_pop.entry(weight_pool(&r.population).to_string()).or_default().push(i);
    }
    let frontier_pooled = rows.iter().any(|r| r.population == "frontier");
    for idx in by_pop.values() {
        let refs: Vec<&Row> = idx.iter().map(|&i| &rows[i]).collect();
        let w = balanced_weights(&refs);
        for (&i, w) in idx.iter().zip(w) {
            rows[i].balanced_weight = w;
            rows[i].weight = w;
            rows[i].cohort_weight = 1.0;
        }
    }
    let total: f64 = rows.iter().map(|r| r.weight).sum();
    let mut by_origin = Mass::new();
    let mut by_label = Mass::new();
    for r in rows.iter() {
        for (m, k) in [(&mut by_origin, r.origin.clone()), (&mut by_label, label(r))] {
            let e = m.entry(k).or_default();
            e.0 += 1;
            e.1 += r.weight;
        }
    }
    let mut out = json!({
        "schema": "slop_ninja_a_bundle_weights_v2", "train": false, "scheme": "evaluation_population_balanced_v1",
        "rule": "evaluation split: weight = balanced_weight computed within each population (equal source-group mass, equal observed origin-cell mass within a group, equal document mass within a cell, scaled to mean 1); cohort weights are not applied",
        "effective_mass": {"by_origin": mass_json(&by_origin, total), "by_label": mass_json(&by_label, total)},
        "populations": by_pop.iter().map(|(k, v)| (k.clone(), json!(v.len()))).collect::<serde_json::Map<_, _>>(),
    });
    if frontier_pooled {
        out["frontier_pooled_with_primary"] = json!("frontier rows are primary documents: one weight pool with population primary");
    }
    out
}
