//! Data gates for A bundles (protocol A2 §4.3, §11.1 G0a–G0c, items 1.13 and
//! 1.17; build B12). `slop_ninja_cc_era gates` stays as it is; this module
//! reuses its feature sets, its cross-fitted adversary (`era_gates::cross_fit`,
//! the λ grid `era_gates::LAMBDAS`) and its era probe, on A bundle rows.
//!
//! **Rows.** Any JSONL of A rows: bundle rows (`slop_ninja_a_bundle_row_v1`
//! or `_v2`), scoring rows or pilot rows. Read fields: `id`, `text`,
//! `origin`, `domain`, `source_group`, and optionally `block` (`T` or `C`;
//! absent means `T`, as in every v1 bundle), `weight`, `source_kind`,
//! `cohort_id` or `cohort`, `collection` and an explicit `source`. The row
//! weight is `weight` (1 when absent) unless a weight field is named
//! (`WeightField::Named`, e.g. `cohort_weight`), which every row must carry.
//!
//! **Sources and classes** (shared by the gates, `layout_audit` and
//! `pilot_gate`). A row's source is `cc` for a C-block row, else from its
//! source kind: `stage3`, `capture`, `raid` (an external-pool row of a
//! `raid-v1-` cohort), `external`, `frontier`, `local`, and for humans
//! `enron` (collection `enron_email_historical`) or `pool`. A row without a
//! source kind is `pool`/`enron` when human and `unspecified` otherwise. Its
//! class (§4.3's list) is `human_<source>` for a human (pooled, RAID, Enron,
//! stage-3, and CC humans) and the origin (`mixed`, `model_only`) for a
//! positive, every source included.
//!
//! **Adversaries** (`cc_era_gate_defs_v2`'s, per scope). Each is
//! `era_gates::cross_fit`: cross-fitted ridge logistic models over source-
//! group folds (`rank(seed, "cc_era_fold_v1:<repeat>", group)`), linear and
//! quantile-binned at λ ∈ {0.1, 1, 10, 100}, the best configuration per
//! repeat, averaged over repeats. Weights restore the 20/55/25 origin mix
//! within the scope (target share / realised share), as the CC gates do; the
//! bundle's own weights are not read. The gain is the Brier improvement over
//! the training folds' base rate, **in Brier units** (§4.3: "Results are in
//! Brier units until B0_ref exists"); with `B0_ref` it is also a share of it.
//! * `layout_typography`: the 43 format features without the
//!   character-class block (gates v2's format gate);
//! * `era_only`: the era features with the era probe's logits (needs the
//!   probe pool; gated on C-block rows only);
//! * `word_count_only`: log words;
//! * diagnostics: `mean_word_length` (MWL: log characters per word) and
//!   `char_class` (the character-class block).
//!
//! The reference-logit (incremental) form of the CC gates needs reference
//! predictions on the rows; Train rows have none, so it is not computed.
//!
//! **Scopes and verdicts.** Block T, block C, and each Tier domain (T rows).
//! The bound is `0.05 × B0_ref` of the scope's block (absolute, S1-3). G0a
//! gates layout/typography and word count on both blocks, era on C, and the
//! same two in every gated Tier domain; G0b layout/typography on both
//! blocks; G0c era on C (the upper bound reported; refused when the rows
//! hold no C-block row). A block scope is always gated; a Tier domain is
//! gated when every domain × origin cell it holds has at least 59 rows
//! (item 1.17, R0's coverage floor, as amended by amendment 17 item 17.1):
//! `human_only` with every human source pooled, `mixed` and `model_only`.
//! The domain × class cells (humans by source) are reported beside it and
//! gate nothing, so one row of a minor human class cannot ungate a domain.
//! Without `B0_ref` the verdict is `pending_b0_ref`: the "before" figures.
//! A run with no gated item is `vacuous`, never a pass.
//!
//! **σ, δ and w** (`seed_pair_sigma`): the method of the cohort-mix analysis
//! (`cohort-mix/ANALYSIS.md`, `5db0b55c…`; `power_mix.py`), with the three
//! seed pairs of item 1.13 (17–29, 17–41, 29–41; the first of a pair is the
//! reference); amendment 12 item 12.3 pins the estimator. Cells are domain
//! × origin; a design at n takes
//! `max(2, round_half_even(n · mix[origin] · dom_given[cell]))` documents
//! per cell, with `dom_given` from the population's row weights, under the
//! self-weighting estimand (each cell weighs its allocation share). **σ is
//! the stratified bootstrap SE of `q = (B_ref − B_s)/B_ref`**, as in
//! `power_mix.py`: B = 2,000 replicates from seed 20261030 (`measure::Rng`,
//! splitmix64), each drawing `alloc_c` documents per cell with replacement,
//! shared by the three systems; per pair the SD (ddof 1) of q over the
//! replicates, pooled over the pairs as `sqrt(mean(se²))`. Reported beside
//! it: the analytic linearised SE `sqrt(Σ_c W_c² var₁(d_c)/alloc_c)` with
//! `d = (L_s − r·L_ref)/B_ref`, `r = B_s/B_ref`, cell means unweighted (as
//! cohort-mix reported both), its continuous-allocation form, and the
//! reward-v1 formula of `slop_ninja_cc_era gates` (`reward_v1_se`). δ =
//! max(0.01, 2σ), w = 2σ of headroom. σ is also given on the manifest's
//! 1e-18 grid (nearest, ties up) and, with the manifest's σ_m, as σ_eff =
//! max(σ_m, σ′) (item 1.2). Every value carries the Kish ESS of the row
//! weights behind it, and the record names the weight field.
//!
//! Outputs hold counts, statistics and digests, never text.
use crate::era_gates::{self, CHAR_CLASS_FEATURES, TARGET_MIX};
use crate::measure::{Rng, file_sha256, percentile};
use crate::store;
use anyhow::{Context, Result, bail, ensure};
use grammar_a_pools::pile::ENRON_COLLECTION;
use grammar_a_pools::splits::rank;
use rayon::prelude::*;
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};

pub const GATES_SCHEMA: &str = "slop_ninja_a_gates_v1";
pub const SIGMA_SCHEMA: &str = "slop_ninja_a_sigma_v1";
/// The rule this module implements for A bundles.
pub const GATES_RULE: &str = "r2_a_bundle_gates_v1";
/// Absolute bound: 0.05 × B0_ref (gates v2's 5%-of-B0 threshold, §11.10).
pub const BOUND_SHARE: f64 = era_gates::ADVERSARY_GATE;
/// R0's realised coverage floor (§11.10): rows per domain × origin cell for
/// a gated domain (item 1.17; amendment 17 item 17.1).
pub const COVERAGE_FLOOR: usize = 59;
/// The gated-domain rule `gates` applies to a Tier domain (recorded).
pub const GATED_DOMAIN_RULE: &str = "PROTOCOL-A2 item 1.17 as amended by amendment 17 item 17.1: a Tier domain is gated when every domain x origin cell it holds (human_only with every human source pooled, mixed, model_only) has at least 59 rows; the domain x class cells (humans by source) are reported and gate nothing; a block is always gated";
/// The CC gates' fold and probe seeds (gates v2's configuration).
pub const DEFAULT_FOLD_SEED: &str = "cc-era-gates-20261031";
pub const DEFAULT_PROBE_SEED: &str = "cc-human-era-probe-20261030";
/// The CC gates' bootstrap seed for adversary gains.
pub const BOOT_SEED: u64 = 0x5eed_e7a2;
/// The RAID cohorts' prefix (R0's `cohort_positive_mass_caps` key).
pub const RAID_COHORT_PREFIX: &str = "raid-v1-";
pub const ORIGINS: [&str; 3] = ["human_only", "mixed", "model_only"];
/// The manifest's grid: values scaled by 1e18.
pub const SCORE_SCALE: u64 = 1_000_000_000_000_000_000;
/// The round-2 recipe's seed pairs (item 1.13), as indices into three systems.
pub const SEED_PAIRS: [(usize, usize); 3] = [(0, 1), (0, 2), (1, 2)];
/// The pinned bootstrap of σ (amendment 12 item 12.3; `power_mix.py`'s B
/// and seed).
pub const SIGMA_DRAWS: usize = 2000;
pub const SIGMA_SEED: u64 = 20261030;
pub const SIGMA_ESTIMATOR: &str = "stratified_bootstrap";
pub const SIGMA_METHOD: &str = "cohort-mix ANALYSIS.md (5db0b55c) power_mix.py, estimator pinned by PROTOCOL-A2 amendment 12 item 12.3: domain x origin cells; allocation max(2, round_half_even(n * mix[origin] * dom_given)), dom_given from the named row-weight field; self-weighting estimand W_c = alloc_c / n_realised, B = sum_c W_c * mean loss; sigma = the stratified bootstrap SE of q = (B_ref - B_s)/B_ref: B = 2000 replicates from seed 20261030 (splitmix64, measure::Rng), each drawing alloc_c documents per cell with replacement, shared by the three systems, the SD (ddof 1) of q over the replicates per seed pair, pooled over the pairs (17-29, 17-41, 29-41) as sqrt(mean(se^2)); reported beside it: the analytic linearised SE sqrt(sum_c W_c^2 var1(d_c) / alloc_c), d = (L_s - r L_ref)/B_ref, r = B_s/B_ref, unweighted cell means, its continuous-allocation form, and the reward-v1 formula of slop_ninja_cc_era gates; delta = max(0.01, 2 sigma); w = 2 sigma of headroom";
/// `slop_ninja_cc_era gates`' reward-v1 σ formula (reported beside σ).
pub const REWARD_V1_FORMULA: &str = "era_gates.rs reward-v1 (reported): row weights w_k = mix[o] * weight_k / origin weight share; B_ref, B_s the w-weighted Brier over all documents, r = B_s/B_ref; pairs of each later seed against the first only (17-29, 17-41); se = sqrt(sum_c (w-share of c) * var1(d_c) / n), d = (L_s - r L_ref)/B_ref; pooled as sqrt(mean(se^2))";

pub const ADV_LAYOUT: &str = "layout_typography";
pub const ADV_ERA: &str = "era_only";
pub const ADV_WORDS: &str = "word_count_only";
pub const ADV_MWL: &str = "mean_word_length";
pub const ADV_CHAR: &str = "char_class";

// ------------------------------------------------------------------ rows

/// A row's block.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Block {
    T,
    C,
}

impl Block {
    pub fn parse(s: &str) -> Result<Block> {
        match s {
            "T" => Ok(Block::T),
            "C" => Ok(Block::C),
            _ => bail!("unknown block {s:?} (T or C)"),
        }
    }
    pub fn id(self) -> &'static str {
        match self {
            Block::T => "T",
            Block::C => "C",
        }
    }
}

#[derive(Deserialize)]
struct RawRow {
    id: String,
    #[serde(default)]
    text: Option<String>,
    origin: String,
    domain: String,
    #[serde(default)]
    source_group: Option<String>,
    #[serde(default)]
    block: Option<String>,
    #[serde(default)]
    weight: Option<f64>,
    #[serde(default)]
    source_kind: Option<String>,
    #[serde(default)]
    cohort_id: Option<String>,
    #[serde(default)]
    cohort: Option<String>,
    #[serde(default)]
    collection: Option<String>,
    #[serde(default)]
    source: Option<String>,
}

/// One A row, as the gates, the audit and the pilot gate read it.
#[derive(Clone, Debug)]
pub struct ARow {
    pub id: String,
    /// Empty when the reader did not keep text.
    pub text: String,
    /// Index into `ORIGINS`.
    pub origin: usize,
    pub domain: String,
    pub group: Option<String>,
    pub block: Block,
    pub weight: f64,
    pub source: String,
    pub class: String,
}

pub fn origin_index(o: &str) -> Result<usize> {
    ORIGINS.iter().position(|x| *x == o).with_context(|| format!("unknown origin {o:?}"))
}

/// A row's source (module notes). `explicit` overrides the derivation.
pub fn source_of(origin: usize, source_kind: Option<&str>, cohort: Option<&str>, collection: Option<&str>, block: Block, explicit: Option<&str>) -> Result<String> {
    if let Some(s) = explicit {
        ensure!(!s.is_empty() && s.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_'), "source {s:?} must be [a-z0-9_]+");
        return Ok(s.to_string());
    }
    if block == Block::C {
        return Ok("cc".into());
    }
    let raid = cohort.is_some_and(|c| c.starts_with(RAID_COHORT_PREFIX));
    let enron = collection == Some(ENRON_COLLECTION);
    let kind = source_kind.map(|k| k.to_ascii_lowercase().replace('_', ""));
    Ok(match kind.as_deref() {
        Some("stage3legacy") => "stage3",
        Some("hostedcapture") => "capture",
        Some("externalpool") if raid => "raid",
        Some("externalpool") => "external",
        Some("frontier") => "frontier",
        Some("localpositive") => "local",
        Some("human") if enron => "enron",
        Some("human") => "pool",
        Some(k) => bail!("unknown source kind {k:?}"),
        None if origin == 0 && enron => "enron",
        None if origin == 0 => "pool",
        None => "unspecified",
    }
    .to_string())
}

/// A row's class: `human_<source>` for a human, the origin for a positive.
pub fn class_of(origin: usize, source: &str) -> String {
    if origin == 0 { format!("human_{source}") } else { ORIGINS[origin].to_string() }
}

/// Which row field carries the row weight.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum WeightField {
    /// `weight`, 1 when absent.
    #[default]
    Default,
    /// A named numeric field that every row must carry.
    Named(String),
}

impl WeightField {
    /// `None` is the default; a name (even `weight`) must be carried by every row.
    pub fn parse(s: Option<&str>) -> Result<WeightField> {
        match s {
            None => Ok(WeightField::Default),
            Some(n) => {
                ensure!(!n.is_empty() && n.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_'), "weight field {n:?} must be [A-Za-z0-9_]+");
                Ok(WeightField::Named(n.to_string()))
            }
        }
    }
    pub fn record(&self) -> Value {
        match self {
            WeightField::Default => json!({"field": "weight", "absent": "1"}),
            WeightField::Named(n) => json!({"field": n, "absent": "refused"}),
        }
    }
}

/// Parses one row; the flag tells whether its block took the default `T`.
fn parse_row(line: &str, keep_text: bool, wf: &WeightField) -> Result<(ARow, bool)> {
    let (r, named): (RawRow, Option<f64>) = match wf {
        WeightField::Default => (serde_json::from_str(line)?, None),
        WeightField::Named(f) => {
            let v: Value = serde_json::from_str(line)?;
            let w = v.get(f).and_then(Value::as_f64).with_context(|| format!("row {}: no numeric weight field {f:?}", v["id"]))?;
            (RawRow::deserialize(&v)?, Some(w))
        }
    };
    let defaulted = r.block.is_none();
    let origin = origin_index(&r.origin)?;
    let block = match r.block.as_deref() {
        None => Block::T,
        Some(b) => Block::parse(b)?,
    };
    let weight = named.unwrap_or(r.weight.unwrap_or(1.0));
    ensure!(weight.is_finite() && weight >= 0.0, "row {}: weight {weight} is not a finite non-negative number", r.id);
    let cohort = r.cohort_id.as_deref().or(r.cohort.as_deref());
    let source = source_of(origin, r.source_kind.as_deref(), cohort, r.collection.as_deref(), block, r.source.as_deref()).with_context(|| format!("row {}", r.id))?;
    let class = class_of(origin, &source);
    let text = if keep_text { r.text.with_context(|| format!("row {}: no text", r.id))? } else { String::new() };
    Ok((ARow { id: r.id, text, origin, domain: r.domain, group: r.source_group, block, weight, source, class }, defaulted))
}

/// Reads A rows (module notes). Refuses duplicate ids. `keep_text` keeps
/// (and requires) each row's text. Returns the rows and how many took the
/// default block `T`.
pub fn read_rows(path: &Path, keep_text: bool) -> Result<(Vec<ARow>, usize)> {
    read_rows_with(path, keep_text, &WeightField::Default, |_| true)
}

/// `read_rows`, keeping only the rows `keep` accepts (every row is parsed
/// and checked; duplicate ids are refused among all rows).
pub fn read_rows_filtered(path: &Path, keep_text: bool, keep: impl Fn(&ARow) -> bool) -> Result<(Vec<ARow>, usize)> {
    read_rows_with(path, keep_text, &WeightField::Default, keep)
}

/// `read_rows_filtered` with the row weight from `wf`.
pub fn read_rows_with(path: &Path, keep_text: bool, wf: &WeightField, keep: impl Fn(&ARow) -> bool) -> Result<(Vec<ARow>, usize)> {
    let r = BufReader::new(std::fs::File::open(path).with_context(|| format!("open {}", path.display()))?);
    let mut rows = Vec::new();
    let mut seen = HashSet::new();
    let mut defaulted = 0usize;
    for (i, l) in r.lines().enumerate() {
        let l = l?;
        if l.trim().is_empty() {
            continue;
        }
        let (row, d) = parse_row(&l, keep_text, wf).with_context(|| format!("{} line {}", path.display(), i + 1))?;
        ensure!(seen.insert(row.id.clone()), "{}: duplicate id {}", path.display(), row.id);
        if keep(&row) {
            defaulted += d as usize;
            rows.push(row);
        }
    }
    Ok((rows, defaulted))
}

/// Weights restoring the 20/55/25 origin mix (target share / realised share),
/// as the CC gates weight a cohort.
pub fn origin_weights(origins: &[usize]) -> Vec<f64> {
    let n = origins.len() as f64;
    let mut c = [0usize; 3];
    for &o in origins {
        c[o] += 1;
    }
    origins.iter().map(|&o| TARGET_MIX[o] / (c[o] as f64 / n)).collect()
}

/// The origin shares of `fixed_base_rate_weights`: humans 0.2 when present;
/// the positive origins present share 0.8, by the 20/55/25 mix among them
/// (exactly 0.55 and 0.25 when both are present, 0.8 for a lone one).
pub fn fixed_base_rate_shares(origins: &[usize]) -> [f64; 3] {
    let mut c = [0usize; 3];
    for &o in origins {
        c[o] += 1;
    }
    let pos_share = 1.0 - TARGET_MIX[0];
    let mut s = [0.0; 3];
    if c[0] > 0 {
        s[0] = TARGET_MIX[0];
    }
    match (c[1] > 0, c[2] > 0) {
        (true, true) => (s[1], s[2]) = (TARGET_MIX[1], TARGET_MIX[2]),
        (true, false) => s[1] = pos_share,
        (false, true) => s[2] = pos_share,
        (false, false) => {}
    }
    s
}

/// Weights giving the humans 0.2 of the mass and the positives 0.8 whatever
/// positive origins are present (`fixed_base_rate_shares`), each origin's
/// share spread evenly over its rows: the pilot gate's two scopes
/// (amendment 12 item 12.1). With all three origins present this is
/// `origin_weights`.
pub fn fixed_base_rate_weights(origins: &[usize]) -> Vec<f64> {
    let n = origins.len() as f64;
    let mut c = [0usize; 3];
    for &o in origins {
        c[o] += 1;
    }
    let s = fixed_base_rate_shares(origins);
    origins.iter().map(|&o| s[o] / (c[o] as f64 / n)).collect()
}

/// Kish's effective sample size, (Σw)² / Σw².
pub fn kish_ess(w: &[f64]) -> f64 {
    let s: f64 = w.iter().sum();
    let s2: f64 = w.iter().map(|x| x * x).sum();
    if s2 > 0.0 { s * s / s2 } else { 0.0 }
}

/// Whether a domain is gated (item 1.17 as amended by amendment 17 item
/// 17.1): every cell it holds has at least `COVERAGE_FLOOR` rows. The cells
/// are the rows' origins (`ORIGINS[row.origin]`, every human source in
/// `human_only`), never their classes. Returns the cells and the flag.
pub fn gated_cells<'a>(origins: impl Iterator<Item = &'a str>) -> (BTreeMap<String, usize>, bool) {
    let mut cells: BTreeMap<String, usize> = BTreeMap::new();
    for c in origins {
        *cells.entry(c.to_string()).or_default() += 1;
    }
    let gated = !cells.is_empty() && cells.values().all(|&n| n >= COVERAGE_FLOOR);
    (cells, gated)
}

// ------------------------------------------------------------------ features

/// Per-row feature sets of the adversaries.
#[derive(Clone, Debug)]
pub struct Feats {
    pub layout: Vec<f64>,
    pub char_class: Vec<f64>,
    pub words: Vec<f64>,
    pub mwl: Vec<f64>,
    pub era: Option<Vec<f64>>,
}

/// The feature names of each set (the era set when computed).
pub fn feature_names(with_era: bool) -> BTreeMap<&'static str, Vec<String>> {
    let (names, _) = era_gates::format_features("");
    let (cc, layout): (Vec<String>, Vec<String>) = names.into_iter().partition(|k| CHAR_CLASS_FEATURES.contains(&k.as_str()));
    let mut m = BTreeMap::from([
        (ADV_LAYOUT, layout),
        (ADV_CHAR, cc),
        (ADV_WORDS, era_gates::word_count_features("").0),
        (ADV_MWL, era_gates::mean_word_length_features("").0),
    ]);
    if with_era {
        m.insert(ADV_ERA, era_gates::era_features("", 0.0, 0.0).0);
    }
    m
}

/// The feature sets of one text; `logits` are the era probe's (raw, masked).
pub fn features(text: &str, logits: Option<(f64, f64)>) -> Feats {
    let (names, v) = era_gates::format_features(text);
    let mut layout = Vec::with_capacity(43);
    let mut char_class = Vec::with_capacity(CHAR_CLASS_FEATURES.len());
    for (k, x) in names.iter().zip(v) {
        if CHAR_CLASS_FEATURES.contains(&k.as_str()) { char_class.push(x) } else { layout.push(x) }
    }
    Feats {
        layout,
        char_class,
        words: era_gates::word_count_features(text).1,
        mwl: era_gates::mean_word_length_features(text).1,
        era: logits.map(|(r, m)| era_gates::era_features(text, r, m).1),
    }
}

/// The era probe of the CC gates (`era_gates::train_probe`), retrained
/// without the given source groups.
pub(crate) fn train_probe(pool: &Path, probe_seed: &str, per_domain_class: usize, exclude: &HashSet<String>) -> Result<(era_gates::Probe, Value)> {
    let a = era_gates::GatesArgs {
        cohort: PathBuf::new(),
        batch: PathBuf::new(),
        pool: pool.to_path_buf(),
        preds: Vec::new(),
        threshold: 0.0,
        out: PathBuf::new(),
        probe_seed: probe_seed.to_string(),
        seed: String::new(),
        draws: 0,
        folds: 0,
        cv_repeats: 0,
        per_domain_class,
        gate_defs: era_gates::GATE_DEFS_V2.into(),
    };
    era_gates::train_probe(&a, exclude)
}

// ------------------------------------------------------------------ adversary

/// Cross-fitting and bootstrap settings (the CC gates' defaults).
#[derive(Clone, Debug)]
pub struct AdvConfig {
    pub seed: String,
    pub folds: usize,
    pub cv_repeats: usize,
    pub draws: usize,
}

impl Default for AdvConfig {
    fn default() -> Self {
        AdvConfig { seed: DEFAULT_FOLD_SEED.into(), folds: 5, cv_repeats: 5, draws: 1000 }
    }
}

impl AdvConfig {
    pub fn record(&self) -> Value {
        json!({"seed": self.seed, "folds": self.folds, "cv_repeats": self.cv_repeats, "draws": self.draws, "lambdas": era_gates::LAMBDAS,
            "fold_scope": "cc_era_fold_v1:<repeat>", "bootstrap_seed": BOOT_SEED, "gate_defs": era_gates::GATE_DEFS_V2})
    }
}

/// One adversary's result in Brier units.
#[derive(Clone, Debug)]
pub struct AdvResult {
    /// Mean over repeats of the best configuration's gain.
    pub gain: f64,
    pub min: f64,
    pub max: f64,
    pub best_config: String,
    /// Stratified bootstrap of repeat 0's best configuration's gain.
    pub boot95: (f64, f64),
    pub repeats: Vec<Value>,
}

impl AdvResult {
    pub fn record(&self, features: usize) -> Value {
        json!({"computed": true, "feature_count": features, "gain_brier": self.gain, "gain_brier_min": self.min, "gain_brier_max": self.max,
            "repeat0_best_config": self.best_config, "repeat0_bootstrap95_brier": [self.boot95.0, self.boot95.1], "upper95_brier": self.boot95.1,
            "repeats": self.repeats})
    }
}

/// Weighted mean Brier loss over `idx`.
pub fn brier(p: &[f64], y: &[f64], w: &[f64], idx: &[usize]) -> f64 {
    let sw: f64 = idx.iter().map(|&i| w[i]).sum();
    idx.iter().map(|&i| w[i] * (p[i] - y[i]).powi(2)).sum::<f64>() / sw
}

fn folds_of(groups: &[&str], seed: &str, rep: usize, k: usize) -> Vec<usize> {
    let scope = format!("cc_era_fold_v1:{rep}");
    groups.iter().map(|g| (rank(seed, &scope, g) % k as u64) as usize).collect()
}

/// Why cross-fitting cannot run on these rows, if it cannot: both labels
/// and positive weight are needed overall and in the training rows of every
/// fold that holds test rows.
fn not_computable(y: &[f64], w: &[f64], folds: &[usize], k: usize) -> Option<String> {
    let n = y.len();
    for f in 0..k {
        if !folds.contains(&f) {
            continue;
        }
        let (mut pos, mut neg) = (0.0, 0.0);
        for i in (0..n).filter(|&i| folds[i] != f) {
            if y[i] > 0.5 { pos += w[i] } else { neg += w[i] }
        }
        if pos <= 0.0 || neg <= 0.0 {
            return Some(format!("fold {f} trains on {} only", if pos > 0.0 { "positives" } else if neg > 0.0 { "humans" } else { "no rows" }));
        }
    }
    None
}

/// The cross-fitted adversary on one feature set (module notes). `cells`
/// stratify the bootstrap (domain × origin within the scope). None, with a
/// reason, when the rows cannot be cross-fitted.
pub fn adversary(feats: &[Vec<f64>], y: &[f64], w: &[f64], groups: &[&str], cells: &[usize], cfg: &AdvConfig) -> Result<std::result::Result<AdvResult, String>> {
    let n = feats.len();
    ensure!(n == y.len() && n == w.len() && n == groups.len() && n == cells.len(), "adversary inputs differ in length");
    ensure!(cfg.folds >= 2 && cfg.cv_repeats >= 1, "need at least 2 folds and 1 repeat");
    if n == 0 {
        return Ok(Err("no rows".into()));
    }
    let all: Vec<usize> = (0..n).collect();
    let mut bests = Vec::with_capacity(cfg.cv_repeats);
    let mut repeats = Vec::with_capacity(cfg.cv_repeats);
    let mut first: Option<(Vec<f64>, Vec<f64>, String)> = None;
    for rep in 0..cfg.cv_repeats {
        let folds = folds_of(groups, &cfg.seed, rep, cfg.folds);
        if let Some(why) = not_computable(y, w, &folds, cfg.folds) {
            return Ok(Err(format!("repeat {rep}: {why}")));
        }
        let (preds, prior) = era_gates::cross_fit(feats, y, w, &folds, cfg.folds)?;
        let bp = brier(&prior, y, w, &all);
        let gains: BTreeMap<String, f64> = preds.iter().map(|(k, p)| (k.clone(), bp - brier(p, y, w, &all))).collect();
        // The best configuration; ties go to the first in key order.
        let (bk, bg) = gains.iter().fold((String::new(), f64::NEG_INFINITY), |acc, (k, &g)| if g > acc.1 { (k.clone(), g) } else { acc });
        bests.push(bg);
        repeats.push(json!({"repeat": rep, "prior_brier": bp, "gain_brier_by_config": gains, "best_gain_brier": bg, "best_config": bk}));
        if first.is_none() {
            let best = preds.get(&bk).with_context(|| format!("repeat {rep}: no configuration gave a finite gain"))?.clone();
            first = Some((best, prior, bk));
        }
    }
    let (best, prior, bk) = first.expect("at least one repeat");
    let lp: Vec<f64> = (0..n).map(|i| w[i] * (prior[i] - y[i]).powi(2)).collect();
    let lb: Vec<f64> = (0..n).map(|i| w[i] * (best[i] - y[i]).powi(2)).collect();
    let mut boot = stratified_bootstrap(cells, cfg.draws, BOOT_SEED, |ix| {
        let sw: f64 = ix.iter().map(|&i| w[i]).sum();
        (ix.iter().map(|&i| lp[i]).sum::<f64>() - ix.iter().map(|&i| lb[i]).sum::<f64>()) / sw
    });
    let boot95 = (percentile(&mut boot, 0.025), percentile(&mut boot, 0.975));
    let gain = bests.iter().sum::<f64>() / bests.len() as f64;
    Ok(Ok(AdvResult {
        gain,
        min: bests.iter().copied().fold(f64::INFINITY, f64::min),
        max: bests.iter().copied().fold(f64::NEG_INFINITY, f64::max),
        best_config: bk,
        boot95,
        repeats,
    }))
}

/// Replicates resampling rows within each cell (sizes kept), drawn in a
/// fixed order from `seed`; `stat` maps a replicate's row indices to a value.
/// Indices are generated per replicate, never stored for all replicates.
pub fn stratified_bootstrap(cells: &[usize], draws: usize, seed: u64, stat: impl Fn(&[usize]) -> f64) -> Vec<f64> {
    let mut by: BTreeMap<usize, Vec<usize>> = BTreeMap::new();
    for (i, &c) in cells.iter().enumerate() {
        by.entry(c).or_default().push(i);
    }
    let cl: Vec<&Vec<usize>> = by.values().collect();
    let mut rng = Rng(seed);
    let mut ix = Vec::with_capacity(cells.len());
    let mut out = Vec::with_capacity(draws);
    for _ in 0..draws {
        ix.clear();
        for c in &cl {
            for _ in 0..c.len() {
                ix.push(c[rng.below(c.len())]);
            }
        }
        out.push(stat(&ix));
    }
    out
}

// ------------------------------------------------------------------ gates

/// Which data gate a run evaluates (§11.1).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Gate {
    G0a,
    G0b,
    G0c,
}

/// A gate scope: a block, or one Tier domain.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Scope {
    Block(Block),
    TierDomain(String),
}

impl Scope {
    pub fn key(&self) -> String {
        match self {
            Scope::Block(b) => format!("block:{}", b.id()),
            Scope::TierDomain(d) => format!("domain:T:{d}"),
        }
    }
    pub fn block(&self) -> Block {
        match self {
            Scope::Block(b) => *b,
            Scope::TierDomain(_) => Block::T,
        }
    }
}

impl Gate {
    pub fn parse(s: &str) -> Result<Gate> {
        match s.to_ascii_lowercase().as_str() {
            "g0a" => Ok(Gate::G0a),
            "g0b" => Ok(Gate::G0b),
            "g0c" => Ok(Gate::G0c),
            _ => bail!("unknown gate {s:?} (G0a, G0b or G0c)"),
        }
    }
    pub fn id(self) -> &'static str {
        match self {
            Gate::G0a => "G0a",
            Gate::G0b => "G0b",
            Gate::G0c => "G0c",
        }
    }
    /// The adversaries this gate bounds in a scope (others are reported). A
    /// Tier domain's set applies only when the domain is gated.
    pub fn gated(self, scope: &Scope) -> &'static [&'static str] {
        match (self, scope) {
            (Gate::G0a, Scope::Block(Block::T)) | (Gate::G0a, Scope::TierDomain(_)) => &[ADV_LAYOUT, ADV_WORDS],
            (Gate::G0a, Scope::Block(Block::C)) => &[ADV_LAYOUT, ADV_ERA, ADV_WORDS],
            (Gate::G0b, Scope::Block(_)) => &[ADV_LAYOUT],
            (Gate::G0c, Scope::Block(Block::C)) => &[ADV_ERA],
            _ => &[],
        }
    }
    /// Whether the era adversary must be computable (C-block rows exist).
    fn needs_era(self) -> bool {
        matches!(self, Gate::G0a | Gate::G0c)
    }
}

/// One gated adversary's verdict: pass iff the gain ≤ 0.05 × B0_ref.
pub fn gate_item(adv: &str, result: Option<&AdvResult>, why: Option<&str>, b0_ref: Option<f64>) -> Value {
    let Some(r) = result else {
        return json!({"adversary": adv, "status": "not_computable", "reason": why});
    };
    match b0_ref {
        None => json!({"adversary": adv, "gain_brier": r.gain, "upper95_brier": r.boot95.1, "status": "pending_b0_ref"}),
        Some(b0) => {
            let bound = BOUND_SHARE * b0;
            let pass = r.gain <= bound;
            json!({"adversary": adv, "gain_brier": r.gain, "upper95_brier": r.boot95.1, "gain_share_of_b0_ref": r.gain / b0, "bound_brier": bound,
                "pass": pass, "upper_bound_also_passes": r.boot95.1 <= bound, "status": if pass { "pass" } else { "fail" }})
        }
    }
}

/// The overall status of gate items: none is vacuous (never a pass); any
/// fail fails; else any pending B0_ref is pending; else any not computable
/// is undetermined; else pass.
pub fn overall(items: &[&Value]) -> &'static str {
    let has = |s: &str| items.iter().any(|v| v["status"] == s);
    if items.is_empty() {
        "vacuous"
    } else if has("fail") {
        "fail"
    } else if has("pending_b0_ref") {
        "pending_b0_ref"
    } else if has("not_computable") {
        "undetermined"
    } else {
        "pass"
    }
}

pub struct GatesArgs {
    pub rows: PathBuf,
    pub pool: Option<PathBuf>,
    pub b0_ref_t: Option<f64>,
    pub b0_ref_c: Option<f64>,
    pub gate: Gate,
    pub out: PathBuf,
    pub cfg: AdvConfig,
    pub probe_seed: String,
    pub per_domain_class: usize,
}

/// Rows of one scope, with their weights and cells.
struct ScopeRows {
    idx: Vec<usize>,
    y: Vec<f64>,
    w: Vec<f64>,
    cells: Vec<usize>,
}

fn scope_rows(rows: &[ARow], idx: Vec<usize>) -> ScopeRows {
    let origins: Vec<usize> = idx.iter().map(|&i| rows[i].origin).collect();
    let w = origin_weights(&origins);
    let y: Vec<f64> = origins.iter().map(|&o| (o != 0) as u8 as f64).collect();
    let mut keys: BTreeMap<(&str, usize), usize> = BTreeMap::new();
    for &i in &idx {
        let n = keys.len();
        keys.entry((rows[i].domain.as_str(), rows[i].origin)).or_insert(n);
    }
    let cells = idx.iter().map(|&i| keys[&(rows[i].domain.as_str(), rows[i].origin)]).collect();
    ScopeRows { idx, y, w, cells }
}

/// Computes every adversary of one scope.
fn scope_adversaries(rows: &[ARow], feats: &[Feats], s: &ScopeRows, cfg: &AdvConfig) -> Result<BTreeMap<&'static str, std::result::Result<AdvResult, String>>> {
    let groups: Vec<&str> = s.idx.iter().map(|&i| rows[i].group.as_deref().expect("groups checked")).collect();
    let mut sets: Vec<(&'static str, Vec<Vec<f64>>)> = vec![
        (ADV_LAYOUT, s.idx.iter().map(|&i| feats[i].layout.clone()).collect()),
        (ADV_WORDS, s.idx.iter().map(|&i| feats[i].words.clone()).collect()),
        (ADV_MWL, s.idx.iter().map(|&i| feats[i].mwl.clone()).collect()),
        (ADV_CHAR, s.idx.iter().map(|&i| feats[i].char_class.clone()).collect()),
    ];
    if s.idx.iter().all(|&i| feats[i].era.is_some()) {
        sets.push((ADV_ERA, s.idx.iter().map(|&i| feats[i].era.clone().expect("era features")).collect()));
    }
    let mut out = BTreeMap::new();
    for (name, f) in sets {
        out.insert(name, adversary(&f, &s.y, &s.w, &groups, &s.cells, cfg)?);
    }
    Ok(out)
}

fn by_count<'a>(it: impl Iterator<Item = &'a str>) -> BTreeMap<String, usize> {
    let mut m = BTreeMap::new();
    for k in it {
        *m.entry(k.to_string()).or_insert(0usize) += 1;
    }
    m
}

/// G0a, G0b or G0c on a set of A rows (module notes). Writes `gates.json`
/// under `out` and returns a summary.
pub fn gates(a: &GatesArgs) -> Result<Value> {
    for (k, b) in [("T", a.b0_ref_t), ("C", a.b0_ref_c)] {
        if let Some(b) = b {
            ensure!(b.is_finite() && b > 0.0, "B0_ref,{k} must be a positive number");
        }
    }
    let (rows, defaulted) = read_rows(&a.rows, true)?;
    ensure!(!rows.is_empty(), "no rows in {}", a.rows.display());
    if let Some(r) = rows.iter().find(|r| r.group.is_none()) {
        bail!("row {} has no source_group (folds are by source group)", r.id);
    }
    let has_c = rows.iter().any(|r| r.block == Block::C);
    ensure!(has_c || a.gate != Gate::G0c, "G0c gates the era adversary on C-block rows, and {} holds none", a.rows.display());
    let groups: HashSet<String> = rows.iter().filter_map(|r| r.group.clone()).collect();
    let (logits, probe_info): (Option<Vec<(f64, f64)>>, Value) = match &a.pool {
        Some(pool) => {
            eprintln!("training the era probe ({} row groups excluded)", groups.len());
            let (probe, info) = train_probe(pool, &a.probe_seed, a.per_domain_class, &groups)?;
            (Some(rows.par_iter().map(|r| probe.logits(&r.text)).collect()), json!({"pool": pool, "pool_sha256": file_sha256(pool)?, "info": info}))
        }
        None => {
            ensure!(!(has_c && a.gate.needs_era()), "{} gates the era adversary on C-block rows: --pool is required", a.gate.id());
            (None, json!({"computed": false, "reason": "no probe pool given; the era adversary is not computed"}))
        }
    };
    eprintln!("features for {} rows", rows.len());
    let feats: Vec<Feats> = rows.par_iter().enumerate().map(|(i, r)| features(&r.text, logits.as_ref().map(|l| l[i]))).collect();
    let names = feature_names(logits.is_some());
    ensure!(names[ADV_LAYOUT].len() == 43, "the layout and typography set must hold 43 features");

    let mut scopes: Vec<(Scope, Vec<usize>)> = Vec::new();
    for b in [Block::T, Block::C] {
        let idx: Vec<usize> = (0..rows.len()).filter(|&i| rows[i].block == b).collect();
        if !idx.is_empty() {
            scopes.push((Scope::Block(b), idx));
        }
    }
    let tier_domains: BTreeSet<&str> = rows.iter().filter(|r| r.block == Block::T).map(|r| r.domain.as_str()).collect();
    for d in tier_domains {
        let idx: Vec<usize> = (0..rows.len()).filter(|&i| rows[i].block == Block::T && rows[i].domain == d).collect();
        scopes.push((Scope::TierDomain(d.to_string()), idx));
    }

    let mut out_scopes = BTreeMap::new();
    let mut items_all: Vec<Value> = Vec::new();
    let mut verdict_blocks = BTreeMap::new();
    let mut verdict_domains = BTreeMap::new();
    for (scope, idx) in scopes {
        eprintln!("adversaries on {} ({} rows)", scope.key(), idx.len());
        let s = scope_rows(&rows, idx);
        let advs = scope_adversaries(&rows, &feats, &s, &a.cfg)?;
        let b0 = match scope.block() {
            Block::T => a.b0_ref_t,
            Block::C => a.b0_ref_c,
        };
        let class_cells = by_count(s.idx.iter().map(|&i| rows[i].class.as_str()));
        let (origin_cells, domain_gated) = gated_cells(s.idx.iter().map(|&i| ORIGINS[rows[i].origin]));
        let gated_here = match &scope {
            Scope::Block(_) => true,
            Scope::TierDomain(_) => domain_gated && a.gate == Gate::G0a,
        };
        let mut adv_rec = BTreeMap::new();
        for (k, r) in &advs {
            let mut v = match r {
                Ok(r) => r.record(names[k].len()),
                Err(why) => json!({"computed": false, "reason": why}),
            };
            if let (Ok(r), Some(b0)) = (r, b0) {
                v["gain_share_of_b0_ref"] = json!(r.gain / b0);
            }
            v["role"] = json!(if gated_here && a.gate.gated(&scope).contains(k) { "gated" } else { "reported" });
            adv_rec.insert(k.to_string(), v);
        }
        if !advs.contains_key(ADV_ERA) {
            adv_rec.insert(ADV_ERA.into(), json!({"computed": false, "reason": "no probe pool given", "role": "reported"}));
        }
        let items: Vec<Value> = if gated_here {
            a.gate.gated(&scope).iter().map(|&k| match advs.get(k) {
                Some(Ok(r)) => gate_item(k, Some(r), None, b0),
                Some(Err(why)) => gate_item(k, None, Some(why), b0),
                None => gate_item(k, None, Some("not computed (no probe pool)"), b0),
            }).collect()
        } else {
            Vec::new()
        };
        let refs: Vec<&Value> = items.iter().collect();
        let status = if items.is_empty() { "not_gated" } else { overall(&refs) };
        let v = json!({"gated": gated_here, "status": status, "items": items});
        match &scope {
            Scope::Block(b) => {
                verdict_blocks.insert(b.id().to_string(), v);
            }
            Scope::TierDomain(d) => {
                verdict_domains.insert(d.clone(), v);
            }
        }
        items_all.extend(items);
        out_scopes.insert(scope.key(), json!({
            "block": scope.block().id(), "rows": s.idx.len(), "by_origin": origin_cells,
            "domain_class_cells": class_cells, "domain_origin_cells": origin_cells, "coverage_floor": COVERAGE_FLOOR, "gated_domain_rule_met": domain_gated,
            "source_groups": s.idx.iter().filter_map(|&i| rows[i].group.as_deref()).collect::<BTreeSet<_>>().len(),
            "kish_ess_origin_weights": kish_ess(&s.w), "b0_ref": b0, "adversaries": adv_rec,
        }));
    }
    let refs: Vec<&Value> = items_all.iter().collect();
    let status = overall(&refs);
    let out = json!({
        "schema": GATES_SCHEMA, "rule": GATES_RULE, "gate": a.gate.id(), "gate_defs": era_gates::GATE_DEFS_V2,
        "verdict": {"status": status, "all_gates_pass": (status == "pass").then_some(true).or((status == "fail").then_some(false)),
            "bound": "gain_brier <= 0.05 x B0_ref of the scope's block", "gated_domain_rule": GATED_DOMAIN_RULE, "b0_ref": {"T": a.b0_ref_t, "C": a.b0_ref_c},
            "blocks": verdict_blocks, "tier_domains": verdict_domains},
        "inputs": {"rows": a.rows, "rows_sha256": file_sha256(&a.rows)?, "rows_count": rows.len(), "rows_default_block_t": defaulted,
            "config": a.cfg.record(), "probe_seed": a.probe_seed, "per_domain_class": a.per_domain_class},
        "probe": probe_info,
        "weights": "within each scope, origin weights restore 20/55/25 (target share / realised share); row weights are not read",
        "features": names.iter().map(|(k, v)| (k.to_string(), json!({"count": v.len(), "names": v}))).collect::<BTreeMap<_, _>>(),
        "feature_ids": {"era": era_gates::ERA_FEATURES_ID, "format": era_gates::FORMAT_FEATURES_ID, "char_class": CHAR_CLASS_FEATURES},
        "rows_by_block_class": by_count(rows.iter().map(|r| format!("{}|{}", r.block.id(), r.class)).collect::<Vec<_>>().iter().map(|s| s.as_str())),
        "scopes": out_scopes,
    });
    store::write_json(&a.out.join("gates.json"), &out)?;
    Ok(json!({"gate": a.gate.id(), "status": status, "rows": rows.len(), "scopes": out["scopes"].as_object().map_or(0, |m| m.len())}))
}

// ------------------------------------------------------------------ sigma

/// Python's `round` (half to even), as `power_mix.py` allocates.
pub fn round_half_even(x: f64) -> f64 {
    x.round_ties_even()
}

/// `x` on the manifest's 1e-18 grid: the nearest integer to x·1e18 (ties
/// up), computed exactly from the binary value of `x` (0 ≤ x ≤ 1).
pub fn to_score_scale(x: f64) -> Result<u64> {
    ensure!(x.is_finite() && (0.0..=1.0).contains(&x), "{x} is outside [0, 1]");
    if x == 0.0 {
        return Ok(0);
    }
    let bits = x.to_bits();
    let exp = ((bits >> 52) & 0x7ff) as i32;
    let frac = bits & ((1u64 << 52) - 1);
    let (m, e) = if exp == 0 { (frac, -1074) } else { (frac | (1u64 << 52), exp - 1075) };
    // x = m · 2^e with e < 0 for x <= 1; m · 1e18 < 2^113.
    let num = u128::from(m) * u128::from(SCORE_SCALE);
    let k = (-e) as u32;
    if k >= 120 {
        return Ok(0);
    }
    let q = num >> k;
    let rem = num - (q << k);
    let q = if rem >= (1u128 << (k - 1)) { q + 1 } else { q };
    Ok(u64::try_from(q)?)
}

/// One domain × origin cell of the σ population.
#[derive(Clone, Debug)]
pub struct Cell {
    pub domain: String,
    pub origin: usize,
    pub docs: Vec<usize>,
    /// Row-weight mass of the cell over the population's.
    pub mass: f64,
}

/// Cells (sorted by domain, origin) and the origin masses of a population.
pub fn cells_of(rows: &[ARow]) -> (Vec<Cell>, [f64; 3]) {
    let total: f64 = rows.iter().map(|r| r.weight).sum();
    let mut by: BTreeMap<(&str, usize), Vec<usize>> = BTreeMap::new();
    for (i, r) in rows.iter().enumerate() {
        by.entry((r.domain.as_str(), r.origin)).or_default().push(i);
    }
    let mut om = [0.0; 3];
    let cells: Vec<Cell> = by
        .into_iter()
        .map(|((d, o), docs)| {
            let mass = docs.iter().map(|&i| rows[i].weight).sum::<f64>() / total;
            om[o] += mass;
            Cell { domain: d.to_string(), origin: o, docs, mass }
        })
        .collect();
    (cells, om)
}

/// A design at n under a mix: allocations and the two estimands' cell weights.
#[derive(Clone, Debug)]
pub struct Design {
    pub alloc: Vec<usize>,
    pub n_realised: usize,
    /// Self-weighting: alloc / n_realised.
    pub w_self: Vec<f64>,
    /// The population's own weighting: the cell's row-weight mass.
    pub w_pop: Vec<f64>,
    /// Continuous allocation at the mix: mix[origin] · dom_given.
    pub w_mix: Vec<f64>,
}

pub fn design(cells: &[Cell], om: &[f64; 3], n: usize, mix: [f64; 3]) -> Result<Design> {
    for (o, &m) in om.iter().enumerate() {
        ensure!(m > 0.0 || mix[o] == 0.0, "origin {} has no weight in the population but mix share {}", ORIGINS[o], mix[o]);
    }
    let dom_given: Vec<f64> = cells.iter().map(|c| c.mass / om[c.origin]).collect();
    let alloc: Vec<usize> = cells.iter().zip(&dom_given).map(|(c, &g)| (round_half_even(n as f64 * mix[c.origin] * g) as usize).max(2)).collect();
    let n_realised: usize = alloc.iter().sum();
    Ok(Design {
        w_self: alloc.iter().map(|&k| k as f64 / n_realised as f64).collect(),
        w_pop: cells.iter().map(|c| c.mass).collect(),
        w_mix: cells.iter().zip(&dom_given).map(|(c, &g)| mix[c.origin] * g).collect(),
        alloc,
        n_realised,
    })
}

fn cell_mean(l: &[f64], docs: &[usize]) -> f64 {
    docs.iter().map(|&i| l[i]).sum::<f64>() / docs.len() as f64
}

fn var1(v: &[f64]) -> f64 {
    if v.len() < 2 {
        return 0.0;
    }
    let m = v.iter().sum::<f64>() / v.len() as f64;
    v.iter().map(|x| (x - m) * (x - m)).sum::<f64>() / (v.len() - 1) as f64
}

/// The analytic linearised SE of q for one pair under cell weights `w`
/// with per-cell sizes `size` (power_mix.py's `an_se`): returns
/// (B_ref, B_entrant, se).
pub fn analytic_se(cells: &[Cell], w: &[f64], size: &[f64], l_ref: &[f64], l_ent: &[f64]) -> (f64, f64, f64) {
    let b0: f64 = cells.iter().zip(w).map(|(c, &wc)| wc * cell_mean(l_ref, &c.docs)).sum();
    let bs: f64 = cells.iter().zip(w).map(|(c, &wc)| wc * cell_mean(l_ent, &c.docs)).sum();
    let r = bs / b0;
    let mut v = 0.0;
    for ((c, &wc), &k) in cells.iter().zip(w).zip(size) {
        let d: Vec<f64> = c.docs.iter().map(|&i| (l_ent[i] - r * l_ref[i]) / b0).collect();
        v += wc * wc * var1(&d) / k;
    }
    (b0, bs, v.sqrt())
}

/// Bootstrap SEs of q for every pair: replicates draw `alloc` documents per
/// cell with replacement (shared by all systems), B = Σ W_c · mean loss.
pub fn bootstrap_se(cells: &[Cell], w: &[f64], alloc: &[usize], losses: &[Vec<f64>], pairs: &[(usize, usize)], draws: usize, seed: u64) -> Vec<f64> {
    let s = losses.len();
    let mut rng = Rng(seed);
    let mut dq: Vec<Vec<f64>> = vec![Vec::with_capacity(draws); pairs.len()];
    let mut b = vec![0.0; s];
    for _ in 0..draws {
        b.iter_mut().for_each(|x| *x = 0.0);
        for ((c, &wc), &k) in cells.iter().zip(w).zip(alloc) {
            let mut sums = vec![0.0; s];
            for _ in 0..k {
                let i = c.docs[rng.below(c.docs.len())];
                for (sys, l) in losses.iter().enumerate() {
                    sums[sys] += l[i];
                }
            }
            for (bs, sm) in b.iter_mut().zip(&sums) {
                *bs += wc * sm / k as f64;
            }
        }
        for (p, &(r, e)) in pairs.iter().enumerate() {
            dq[p].push((b[r] - b[e]) / b[r]);
        }
    }
    dq.iter().map(|v| var1(v).sqrt()).collect()
}

/// `slop_ninja_cc_era gates`' reward-v1 SE of q for one pair (`era_gates`'
/// "sigma, delta, w" block, reported beside σ): row weights `w_k = mix[o] ·
/// weight_k / om[o]` (the CC gates' target share / realised share when the
/// row weights are 1), B_ref and B_s the w-weighted Brier over all
/// documents, `r = B_s/B_ref`, and `v = Σ_c (w-share of c) · var₁(d_c) / n`
/// with `d = (L_s − r·L_ref)/B_ref`. Returns (B_ref, B_entrant, se).
pub fn reward_v1_se(rows: &[ARow], cells: &[Cell], om: &[f64; 3], mix: [f64; 3], n: usize, l_ref: &[f64], l_ent: &[f64]) -> (f64, f64, f64) {
    let w: Vec<f64> = rows.iter().map(|r| if om[r.origin] > 0.0 { mix[r.origin] * r.weight / om[r.origin] } else { 0.0 }).collect();
    let sw: f64 = w.iter().sum();
    let b0 = w.iter().zip(l_ref).map(|(a, b)| a * b).sum::<f64>() / sw;
    let bs = w.iter().zip(l_ent).map(|(a, b)| a * b).sum::<f64>() / sw;
    let r = bs / b0;
    let mut v = 0.0;
    for c in cells {
        let wc = c.docs.iter().map(|&i| w[i]).sum::<f64>() / sw;
        let d: Vec<f64> = c.docs.iter().map(|&i| (l_ent[i] - r * l_ref[i]) / b0).collect();
        v += wc * var1(&d) / n as f64;
    }
    (b0, bs, v.sqrt())
}

/// Reads predictions: involvement is `1 − p_human` (the Tier C evaluation's
/// rule) or `prediction.ai_confidence_percent / 100`.
pub fn read_predictions(path: &Path) -> Result<BTreeMap<String, f64>> {
    let r = BufReader::new(std::fs::File::open(path).with_context(|| format!("open {}", path.display()))?);
    let mut m = BTreeMap::new();
    for (i, l) in r.lines().enumerate() {
        let l = l?;
        if l.trim().is_empty() {
            continue;
        }
        let v: Value = serde_json::from_str(&l).with_context(|| format!("{} line {}", path.display(), i + 1))?;
        let id = v["id"].as_str().with_context(|| format!("{} line {}: id", path.display(), i + 1))?.to_string();
        let p = if let Some(h) = v["p_human"].as_f64() {
            1.0 - h
        } else if let Some(c) = v["prediction"]["ai_confidence_percent"].as_f64() {
            c / 100.0
        } else {
            bail!("{} line {}: neither p_human nor prediction.ai_confidence_percent", path.display(), i + 1);
        };
        ensure!(p.is_finite() && (-1e-12..=1.0 + 1e-12).contains(&p), "{}: involvement {p} for {id} is outside [0, 1]", path.display());
        ensure!(m.insert(id.clone(), p.clamp(0.0, 1.0)).is_none(), "{}: duplicate id {id}", path.display());
    }
    Ok(m)
}

/// The designs and bootstrap of a σ run.
#[derive(Clone, Debug)]
pub struct SigmaOpts {
    /// Batch sizes (1,000 for σ′, 4,000 for σ_C).
    pub ns: Vec<usize>,
    pub mix: [f64; 3],
    pub draws: usize,
    pub seed: u64,
    /// The manifest's σ_m on its 1e-18 grid, for σ_eff.
    pub sigma_m_scaled: Option<u64>,
}

pub struct SigmaArgs {
    pub rows: PathBuf,
    /// NAME=PATH for exactly three seeds, in the order 17, 29, 41.
    pub preds: Vec<(String, PathBuf)>,
    pub block: Option<Block>,
    /// The row-weight field behind `dom_given` and the Kish ESS.
    pub weight_field: WeightField,
    pub opts: SigmaOpts,
    pub out: PathBuf,
}

/// σ, δ and w from three seeds by the cohort-mix method (module notes),
/// with the Kish ESS behind every value. Pure: the rows and the per-system
/// losses (systems × rows) are given. σ is the stratified bootstrap SE.
pub fn seed_pair_sigma(rows: &[ARow], names: &[String], losses: &[Vec<f64>], opts: &SigmaOpts) -> Result<Value> {
    let SigmaOpts { ns, mix, draws, seed, sigma_m_scaled } = opts.clone();
    ensure!(losses.len() == 3 && names.len() == 3, "σ needs exactly three seeds");
    ensure!(losses.iter().all(|l| l.len() == rows.len()), "losses and rows differ in length");
    ensure!((mix.iter().sum::<f64>() - 1.0).abs() < 1e-9 && mix.iter().all(|&m| m >= 0.0), "the mix must be shares summing to 1");
    ensure!(draws >= 2, "the bootstrap needs at least 2 draws");
    let (cells, om) = cells_of(rows);
    let weights: Vec<f64> = rows.iter().map(|r| r.weight).collect();
    let small = cells.iter().filter(|c| c.docs.len() < 2).count();
    let mut designs = Vec::new();
    for &n in &ns {
        ensure!(n > 0, "n must be positive");
        let d = design(&cells, &om, n, mix)?;
        let size: Vec<f64> = d.alloc.iter().map(|&k| k as f64).collect();
        let boot = bootstrap_se(&cells, &d.w_self, &d.alloc, losses, &SEED_PAIRS, draws, seed);
        let mut pairs = BTreeMap::new();
        let mut se2_an = Vec::new();
        let mut se2_boot = Vec::new();
        let mut se2_mix = Vec::new();
        let mut se2_pop = Vec::new();
        for (p, &(r, e)) in SEED_PAIRS.iter().enumerate() {
            let (b0, bs, se) = analytic_se(&cells, &d.w_self, &size, &losses[r], &losses[e]);
            let nmix: Vec<f64> = d.w_mix.iter().map(|&wc| n as f64 * wc).collect();
            let (_, _, se_mix) = analytic_se(&cells, &d.w_mix, &nmix, &losses[r], &losses[e]);
            let (b0_pop, bs_pop, se_pop) = analytic_se(&cells, &d.w_pop, &size, &losses[r], &losses[e]);
            se2_an.push(se * se);
            se2_boot.push(boot[p] * boot[p]);
            se2_mix.push(se_mix * se_mix);
            se2_pop.push(se_pop * se_pop);
            pairs.insert(format!("{}-{}", names[r], names[e]), json!({
                "reference": names[r], "entrant": names[e], "b_reference": b0, "b_entrant": bs, "q_point": (b0 - bs) / b0,
                "se_bootstrap": boot[p], "se_analytic": se, "se_analytic_continuous_allocation": se_mix,
                "population_estimand": {"b_reference": b0_pop, "q_point": (b0_pop - bs_pop) / b0_pop, "se_analytic": se_pop},
            }));
        }
        let pool = |v: &[f64]| (v.iter().sum::<f64>() / v.len() as f64).sqrt();
        // σ is the bootstrap SE (amendment 12 item 12.3).
        let sigma = pool(&se2_boot);
        let delta = (2.0 * sigma).max(0.01);
        // The reward-v1 formula of `slop_ninja_cc_era gates`, reported: each later seed against the first.
        let mut rv1 = BTreeMap::new();
        let mut se2_rv1 = Vec::new();
        for e in 1..losses.len() {
            let (b0, bs, se) = reward_v1_se(rows, &cells, &om, mix, n, &losses[0], &losses[e]);
            se2_rv1.push(se * se);
            rv1.insert(format!("{}-{}", names[0], names[e]), json!({"b_reference": b0, "b_entrant": bs, "q_point": (b0 - bs) / b0, "se": se}));
        }
        let b0_first = cells.iter().zip(&d.w_self).map(|(c, &wc)| wc * cell_mean(&losses[0], &c.docs)).sum::<f64>();
        let sigma_scaled = to_score_scale(sigma)?;
        let eff = sigma_m_scaled.map(|m| {
            let e = m.max(sigma_scaled);
            json!({"sigma_m_scaled": m.to_string(), "sigma_eff_scaled": e.to_string(),
                "delta_eff_scaled": (u128::from(SCORE_SCALE / 100)).max(2 * u128::from(e)).to_string(), "rule": "sigma_eff = max(sigma_m, sigma'); delta = max(0.01, 2 sigma_eff) (item 1.2)"})
        });
        let kish_design = cells.iter().zip(&d.w_pop).zip(&d.alloc).map(|((_, &wp), &k)| wp * wp / (k as f64 / d.n_realised as f64)).sum::<f64>();
        designs.push(json!({
            "n": n, "mix": mix, "n_realised": d.n_realised,
            "alloc": cells.iter().zip(&d.alloc).map(|(c, &k)| (format!("{}|{}", c.domain, ORIGINS[c.origin]), k)).collect::<BTreeMap<_, _>>(),
            "pairs": pairs,
            "sigma": sigma, "sigma_estimator": SIGMA_ESTIMATOR, "sigma_analytic": pool(&se2_an), "sigma_analytic_continuous_allocation": pool(&se2_mix),
            "reward_v1_cc_gates_formula": {"role": "reported", "formula": REWARD_V1_FORMULA, "pairs": rv1, "sigma": pool(&se2_rv1)},
            "delta": delta, "w_share_of_headroom": 2.0 * sigma, "w_brier_at_first_seed": 2.0 * sigma * b0_first,
            "epsilon_brier_at_first_seed": 2.0 * sigma * b0_first / 250.0, "b_first_seed": b0_first,
            "sigma_scaled": sigma_scaled.to_string(), "sigma_eff": eff,
            "population_estimand": {"sigma": pool(&se2_pop), "kish_design_effect_vs_self_weighting": kish_design},
        }));
    }
    let mut by_origin = BTreeMap::new();
    for (o, name) in ORIGINS.iter().enumerate() {
        let w: Vec<f64> = rows.iter().filter(|r| r.origin == o).map(|r| r.weight).collect();
        by_origin.insert(name.to_string(), json!({"rows": w.len(), "weight_mass": om[o], "kish_ess": kish_ess(&w)}));
    }
    Ok(json!({
        "schema": SIGMA_SCHEMA, "method": SIGMA_METHOD, "systems": names, "seed_pairs": SEED_PAIRS.iter().map(|&(r, e)| format!("{}-{}", names[r], names[e])).collect::<Vec<_>>(),
        "rows": rows.len(), "kish_ess_row_weights": kish_ess(&weights), "by_origin": by_origin,
        "cells": cells.len(), "cells_below_two_documents": small,
        "bootstrap": {"draws": draws, "seed": seed, "rng": "splitmix64 (measure::Rng)", "pinned": {"draws": SIGMA_DRAWS, "seed": SIGMA_SEED}, "as_pinned": draws == SIGMA_DRAWS && seed == SIGMA_SEED},
        "rounding": "sigma_scaled = nearest integer to sigma x 1e18 (ties up), exact from the f64 value",
        "designs": designs,
    }))
}

/// The `sigma` command: reads rows and three prediction files, writes
/// `sigma.json` under `out`.
pub fn sigma(a: &SigmaArgs) -> Result<Value> {
    ensure!(a.preds.len() == 3, "--pred is needed exactly three times (seeds 17, 29, 41 in order)");
    let (rows, _) = read_rows_with(&a.rows, false, &a.weight_field, |r| a.block.is_none_or(|b| r.block == b))?;
    ensure!(!rows.is_empty(), "no rows in scope");
    let mut names = Vec::new();
    let mut losses = Vec::new();
    let mut files = BTreeMap::new();
    for (name, path) in &a.preds {
        let p = read_predictions(path)?;
        let l: Vec<f64> = rows
            .iter()
            .map(|r| p.get(&r.id).map(|&x| (x - (r.origin != 0) as u8 as f64).powi(2)).with_context(|| format!("{name}: no prediction for {}", r.id)))
            .collect::<Result<_>>()?;
        files.insert(name.clone(), json!({"path": path, "sha256": file_sha256(path)?}));
        names.push(name.clone());
        losses.push(l);
    }
    let mut out = seed_pair_sigma(&rows, &names, &losses, &a.opts)?;
    out["inputs"] = json!({"rows": a.rows, "rows_sha256": file_sha256(&a.rows)?, "block": a.block.map(Block::id), "weight_field": a.weight_field.record(), "predictions": files});
    store::write_json(&a.out.join("sigma.json"), &out)?;
    Ok(json!({"rows": rows.len(), "designs": out["designs"].as_array().map(|d| d.iter().map(|x| json!({"n": x["n"], "sigma": x["sigma"], "sigma_scaled": x["sigma_scaled"]})).collect::<Vec<_>>())}))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(id: &str, origin: usize, domain: &str, group: &str, text: &str) -> ARow {
        ARow { id: id.into(), text: text.into(), origin, domain: domain.into(), group: Some(group.into()), block: Block::T, weight: 1.0, source: "pool".into(), class: class_of(origin, if origin == 0 { "pool" } else { "local" }) }
    }

    #[test]
    fn sources_and_classes_follow_the_taxonomy() {
        let s = |o, k: Option<&str>, c: Option<&str>, col: Option<&str>, b| source_of(o, k, c, col, b, None).unwrap();
        assert_eq!(s(0, Some("human"), None, None, Block::T), "pool");
        assert_eq!(s(0, Some("human"), None, Some(ENRON_COLLECTION), Block::T), "enron");
        assert_eq!(s(0, Some("Human"), None, Some(ENRON_COLLECTION), Block::T), "enron");
        assert_eq!(s(0, Some("external_pool"), Some("raid-v1-paired"), None, Block::T), "raid");
        assert_eq!(s(0, Some("ExternalPool"), Some("raid-v1-human-paraphrase"), None, Block::T), "raid");
        assert_eq!(s(1, Some("external_pool"), Some("other-pool"), None, Block::T), "external");
        assert_eq!(s(0, Some("stage3_legacy"), Some("stage3_legacy_train"), None, Block::T), "stage3");
        assert_eq!(s(2, Some("hosted_capture"), None, None, Block::T), "capture");
        assert_eq!(s(1, Some("frontier"), None, None, Block::T), "frontier");
        assert_eq!(s(2, Some("local_positive"), None, Some(ENRON_COLLECTION), Block::T), "local");
        assert_eq!(s(0, Some("human"), None, None, Block::C), "cc");
        assert_eq!(s(0, None, None, None, Block::T), "pool");
        assert_eq!(s(1, None, None, None, Block::T), "unspecified");
        assert!(source_of(0, Some("mystery"), None, None, Block::T, None).is_err());
        assert_eq!(source_of(1, Some("frontier"), None, None, Block::T, Some("spliced")).unwrap(), "spliced");
        assert!(source_of(1, None, None, None, Block::T, Some("Bad Source")).is_err());
        assert_eq!(class_of(0, "raid"), "human_raid");
        assert_eq!(class_of(0, "stage3"), "human_stage3");
        assert_eq!(class_of(1, "raid"), "mixed");
        assert_eq!(class_of(2, "capture"), "model_only");
    }

    #[test]
    fn rows_parse_with_defaults_and_refuse_bad_input() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("rows.jsonl");
        std::fs::write(&p, concat!(
            r#"{"id":"a","text":"x y","origin":"human_only","domain":"d","source_group":"g1","source_kind":"human","weight":2.5}"#, "\n",
            r#"{"id":"b","text":"x y","origin":"mixed","domain":"d","source_group":"g1","block":"C","cohort_id":"cc_n2"}"#, "\n",
        )).unwrap();
        let (rows, defaulted) = read_rows(&p, true).unwrap();
        assert_eq!(defaulted, 1);
        assert_eq!((rows[0].block, rows[0].weight, rows[0].class.as_str()), (Block::T, 2.5, "human_pool"));
        assert_eq!((rows[1].block, rows[1].weight, rows[1].source.as_str(), rows[1].class.as_str()), (Block::C, 1.0, "cc", "mixed"));
        std::fs::write(&p, concat!(r#"{"id":"a","text":"x","origin":"human_only","domain":"d"}"#, "\n", r#"{"id":"a","text":"x","origin":"human_only","domain":"d"}"#, "\n")).unwrap();
        assert!(read_rows(&p, true).unwrap_err().to_string().contains("duplicate id"));
        std::fs::write(&p, r#"{"id":"a","text":"x","origin":"human","domain":"d"}"#).unwrap();
        assert!(read_rows(&p, true).is_err());
        std::fs::write(&p, r#"{"id":"a","origin":"human_only","domain":"d"}"#).unwrap();
        assert!(read_rows(&p, true).is_err());
        assert!(read_rows(&p, false).is_ok());
        std::fs::write(&p, r#"{"id":"a","origin":"human_only","domain":"d","block":"X"}"#).unwrap();
        assert!(read_rows(&p, false).is_err());
        std::fs::write(&p, r#"{"id":"a","origin":"human_only","domain":"d","weight":-1}"#).unwrap();
        assert!(read_rows(&p, false).is_err());
    }

    #[test]
    fn origin_weights_restore_the_mix_and_kish_ess() {
        // 10 humans, 10 mixed, 20 model-only.
        let o: Vec<usize> = std::iter::repeat_n(0, 10).chain(std::iter::repeat_n(1, 10)).chain(std::iter::repeat_n(2, 20)).collect();
        let w = origin_weights(&o);
        let tot: f64 = w.iter().sum();
        let share = |k: usize| o.iter().zip(&w).filter(|(x, _)| **x == k).map(|(_, w)| w).sum::<f64>() / tot;
        assert!((share(0) - 0.20).abs() < 1e-12 && (share(1) - 0.55).abs() < 1e-12 && (share(2) - 0.25).abs() < 1e-12);
        assert!((w[0] - 0.8).abs() < 1e-12 && (w[10] - 2.2).abs() < 1e-12 && (w[20] - 0.5).abs() < 1e-12);
        assert_eq!(kish_ess(&[1.0; 7]), 7.0);
        // (1 + 1 + 2)^2 / (1 + 1 + 4) = 16 / 6.
        assert!((kish_ess(&[1.0, 1.0, 2.0]) - 16.0 / 6.0).abs() < 1e-12);
    }

    #[test]
    fn the_layout_set_is_43_features_without_the_char_class_block() {
        let names = feature_names(true);
        assert_eq!(names[ADV_LAYOUT].len(), 43);
        assert_eq!(names[ADV_CHAR].len(), CHAR_CLASS_FEATURES.len());
        assert!(names[ADV_LAYOUT].iter().all(|k| !CHAR_CLASS_FEATURES.contains(&k.as_str())));
        assert_eq!(names[ADV_WORDS], vec!["ln_words".to_string()]);
        assert_eq!(names[ADV_MWL], vec!["ln_chars_per_word".to_string()]);
        let f = features("Some text here.  \nAnother line", Some((0.1, 0.2)));
        assert_eq!((f.layout.len(), f.char_class.len(), f.words.len(), f.mwl.len()), (43, 8, 1, 1));
        assert_eq!(f.era.as_ref().unwrap().len(), names[ADV_ERA].len());
        // The split preserves order and values of the format features.
        let (n, v) = era_gates::format_features("Some text here.  \nAnother line");
        let lay: Vec<f64> = n.iter().zip(&v).filter(|(k, _)| !CHAR_CLASS_FEATURES.contains(&k.as_str())).map(|(_, x)| *x).collect();
        assert_eq!(lay, f.layout);
        assert!(features("x", None).era.is_none());
    }

    #[test]
    fn the_coverage_floor_decides_gated_domains() {
        let mut origins = vec!["human_only"; 59];
        origins.extend(vec!["mixed"; 59]);
        let (cells, g) = gated_cells(origins.iter().copied());
        assert!(g);
        assert_eq!(cells["mixed"], 59);
        origins.extend(vec!["model_only"; 58]);
        assert!(!gated_cells(origins.iter().copied()).1);
        assert!(!gated_cells(std::iter::empty()).1);
    }

    /// Rows where trailing whitespace marks every positive, with words drawn
    /// so the word count carries no signal.
    fn layout_cue_rows(n: usize, cue: bool) -> Vec<ARow> {
        let mut rng = Rng(7);
        (0..n)
            .map(|i| {
                let origin = [0, 1, 1, 2][i % 4];
                let words: Vec<String> = (0..20 + rng.below(30)).map(|k| format!("w{}", (k * 7 + i) % 13)).collect();
                let mut t = format!("{}.\n{}.", words[..words.len() / 2].join(" "), words[words.len() / 2..].join(" "));
                if cue && origin != 0 {
                    t = t.replace(".\n", ".  \n");
                }
                row(&format!("r{i}"), origin, "d", &format!("g{}", i / 2), &t)
            })
            .collect()
    }

    fn run_adv(rows: &[ARow], pick: fn(&Feats) -> Vec<f64>) -> AdvResult {
        let feats: Vec<Feats> = rows.iter().map(|r| features(&r.text, None)).collect();
        let s = scope_rows(rows, (0..rows.len()).collect());
        let groups: Vec<&str> = rows.iter().map(|r| r.group.as_deref().unwrap()).collect();
        let f: Vec<Vec<f64>> = feats.iter().map(pick).collect();
        let cfg = AdvConfig { draws: 50, cv_repeats: 2, ..AdvConfig::default() };
        adversary(&f, &s.y, &s.w, &groups, &s.cells, &cfg).unwrap().unwrap()
    }

    #[test]
    fn a_layout_cue_gives_the_layout_adversary_a_large_brier_gain() {
        let rows = layout_cue_rows(240, true);
        let r = run_adv(&rows, |f| f.layout.clone());
        // Weighted base rate 0.8 under 20/55/25: the prior's Brier is 0.16, and
        // a perfect cue removes nearly all of it.
        assert!(r.gain > 0.14 && r.gain < 0.2, "{}", r.gain);
        assert!(r.boot95.0 <= r.gain + 1e-9 && r.boot95.1 >= r.boot95.0);
        assert_eq!(r.repeats.len(), 2);
        let none = run_adv(&layout_cue_rows(240, false), |f| f.layout.clone());
        assert!(none.gain < 0.02, "{}", none.gain);
        let words = run_adv(&rows, |f| f.words.clone());
        assert!(words.gain < 0.02, "{}", words.gain);
    }

    #[test]
    fn rows_that_cannot_be_cross_fitted_are_not_computable() {
        let rows: Vec<ARow> = (0..20).map(|i| row(&format!("r{i}"), if i < 10 { 0 } else { 1 }, "d", if i < 10 { "ga" } else { "gb" }, "a b c.")).collect();
        let s = scope_rows(&rows, (0..rows.len()).collect());
        let groups: Vec<&str> = rows.iter().map(|r| r.group.as_deref().unwrap()).collect();
        let f: Vec<Vec<f64>> = rows.iter().map(|r| features(&r.text, None).words).collect();
        // Two groups, one per label: some fold trains on one label only.
        let r = adversary(&f, &s.y, &s.w, &groups, &s.cells, &AdvConfig::default()).unwrap();
        assert!(r.is_err(), "{r:?}");
    }

    #[test]
    fn verdicts_use_the_absolute_bound() {
        let r = AdvResult { gain: 0.004, min: 0.0, max: 0.0, best_config: "linear_l1".into(), boot95: (0.002, 0.006), repeats: vec![] };
        let pending = gate_item(ADV_LAYOUT, Some(&r), None, None);
        assert_eq!(pending["status"], "pending_b0_ref");
        // 0.05 x 0.08 = 0.004: equal passes, the upper bound does not.
        let at = gate_item(ADV_LAYOUT, Some(&r), None, Some(0.08));
        assert_eq!((at["status"].as_str(), at["upper_bound_also_passes"].as_bool()), (Some("pass"), Some(false)));
        let fail = gate_item(ADV_LAYOUT, Some(&r), None, Some(0.0799));
        assert_eq!(fail["status"], "fail");
        let nc = gate_item(ADV_ERA, None, Some("no rows"), Some(0.08));
        assert_eq!(nc["status"], "not_computable");
        assert_eq!(overall(&[&at, &pending, &nc]), "pending_b0_ref");
        assert_eq!(overall(&[&at, &fail, &pending]), "fail");
        assert_eq!(overall(&[&at, &nc]), "undetermined");
        assert_eq!(overall(&[&at]), "pass");
        let t = Scope::Block(Block::T);
        let c = Scope::Block(Block::C);
        let d = Scope::TierDomain("x".into());
        assert_eq!(Gate::G0a.gated(&t), &[ADV_LAYOUT, ADV_WORDS]);
        assert_eq!(Gate::G0a.gated(&c), &[ADV_LAYOUT, ADV_ERA, ADV_WORDS]);
        assert_eq!(Gate::G0a.gated(&d), &[ADV_LAYOUT, ADV_WORDS]);
        assert_eq!(Gate::G0b.gated(&c), &[ADV_LAYOUT]);
        assert!(Gate::G0b.gated(&d).is_empty());
        assert_eq!(Gate::G0c.gated(&c), &[ADV_ERA]);
        assert!(Gate::G0c.gated(&t).is_empty());
        assert_eq!(d.key(), "domain:T:x");
    }

    #[test]
    fn gates_end_to_end_on_a_synthetic_bundle() {
        let dir = tempfile::tempdir().unwrap();
        let rows = layout_cue_rows(400, true);
        let mut s = String::new();
        for r in &rows {
            s.push_str(&serde_json::to_string(&json!({"id": r.id, "text": r.text, "origin": ORIGINS[r.origin], "domain": if r.id.ends_with('7') { "e" } else { "d" },
                "source_group": r.group, "source_kind": if r.origin == 0 { "human" } else { "local_positive" }})).unwrap());
            s.push('\n');
        }
        let p = dir.path().join("rows.jsonl");
        std::fs::write(&p, &s).unwrap();
        let run = |b0: Option<f64>| {
            let a = GatesArgs { rows: p.clone(), pool: None, b0_ref_t: b0, b0_ref_c: None, gate: Gate::G0a, out: dir.path().to_path_buf(),
                cfg: AdvConfig { draws: 20, cv_repeats: 1, ..AdvConfig::default() }, probe_seed: DEFAULT_PROBE_SEED.into(), per_domain_class: 10 };
            gates(&a).unwrap();
            serde_json::from_str::<Value>(&std::fs::read_to_string(dir.path().join("gates.json")).unwrap()).unwrap()
        };
        let v = run(None);
        assert_eq!(v["verdict"]["status"], "pending_b0_ref");
        let d = &v["scopes"]["domain:T:d"];
        assert_eq!(d["gated_domain_rule_met"], true);
        assert_eq!(v["scopes"]["domain:T:e"]["gated_domain_rule_met"], false);
        assert_eq!(v["verdict"]["tier_domains"]["e"]["status"], "not_gated");
        assert_eq!(d["adversaries"][ADV_LAYOUT]["role"], "gated");
        assert_eq!(d["adversaries"][ADV_MWL]["role"], "reported");
        assert_eq!(d["adversaries"][ADV_ERA]["computed"], false);
        assert_eq!(v["features"][ADV_LAYOUT]["count"], 43);
        // A large layout gain fails at a realistic B0_ref.
        let f = run(Some(0.05));
        assert_eq!(f["verdict"]["status"], "fail");
        assert_eq!(f["verdict"]["blocks"]["T"]["items"][0]["status"], "fail");
        // Output holds no text: no synthetic word appears in it.
        let raw = std::fs::read_to_string(dir.path().join("gates.json")).unwrap();
        let frag: String = rows[5].text.chars().take(14).collect();
        assert!(frag.contains(' ') && !raw.contains(&frag), "{frag}");
    }

    #[test]
    fn score_scale_is_exact_and_rounds_to_nearest() {
        assert_eq!(to_score_scale(0.0).unwrap(), 0);
        assert_eq!(to_score_scale(1.0).unwrap(), SCORE_SCALE);
        assert_eq!(to_score_scale(0.5).unwrap(), SCORE_SCALE / 2);
        // 0.035 is 0.0350000000000000033306690738754696… in binary.
        assert_eq!(to_score_scale(0.035).unwrap(), 35_000_000_000_000_003);
        // 0.01 is 0.01000000000000000020816681711721685… → 10^16.
        assert_eq!(to_score_scale(0.01).unwrap(), 10_000_000_000_000_000);
        // 2^-60 · 1e18 = 0.867… → 1; 2^-61 · 1e18 = 0.4336… → 0.
        assert_eq!(to_score_scale(2f64.powi(-60)).unwrap(), 1);
        assert_eq!(to_score_scale(2f64.powi(-61)).unwrap(), 0);
        assert_eq!(to_score_scale(f64::MIN_POSITIVE).unwrap(), 0);
        // An exact tie: 2^-19 · 1e18 = 1,907,348,632,812.5 rounds up.
        assert_eq!(to_score_scale(2f64.powi(-19)).unwrap(), 1_907_348_632_813);
        assert!(to_score_scale(1.5).is_err() && to_score_scale(f64::NAN).is_err());
    }

    #[test]
    fn allocation_rounds_half_to_even_as_python_does() {
        assert_eq!(round_half_even(2.5), 2.0);
        assert_eq!(round_half_even(3.5), 4.0);
        assert_eq!(round_half_even(0.5), 0.0);
        assert_eq!(round_half_even(2.4999), 2.0);
    }

    /// An independent replay of the pinned bootstrap stream for the three
    /// seed pairs: cells are (first document, allocation, W) over two
    /// consecutive documents each, in cell order; per replicate and cell,
    /// `alloc` draws of `Rng::below(2)`; B = Σ W · mean; SD with ddof 1.
    fn replay_bootstrap(losses: &[Vec<f64>], cells: &[(usize, usize, f64)], draws: usize, seed: u64) -> Vec<f64> {
        let mut rng = Rng(seed);
        let mut dq = vec![Vec::new(); 3];
        for _ in 0..draws {
            let mut b = [0.0f64; 3];
            for &(start, k, wc) in cells {
                let mut sums = [0.0f64; 3];
                for _ in 0..k {
                    let i = start + rng.below(2);
                    for s in 0..3 {
                        sums[s] += losses[s][i];
                    }
                }
                for s in 0..3 {
                    b[s] += wc * sums[s] / k as f64;
                }
            }
            for (p, &(r, e)) in [(0usize, 1usize), (0, 2), (1, 2)].iter().enumerate() {
                dq[p].push((b[r] - b[e]) / b[r]);
            }
        }
        dq.iter()
            .map(|v| {
                let m = v.iter().sum::<f64>() / v.len() as f64;
                (v.iter().map(|x| (x - m) * (x - m)).sum::<f64>() / (v.len() - 1) as f64).sqrt()
            })
            .collect()
    }

    /// The known-answer fixture: one domain, two docs per origin, unit
    /// weights, losses chosen so every term is a hand-computable fraction.
    #[test]
    fn sigma_delta_and_w_on_a_known_answer_fixture() {
        let rows: Vec<ARow> = (0..6).map(|i| row(&format!("r{i}"), i / 2, "d", &format!("g{i}"), "")).collect();
        // Seed A (reference of 17-29, 17-41) and seeds B, C.
        let la = vec![0.10, 0.30, 0.20, 0.20, 0.40, 0.00];
        let lb = vec![0.20, 0.20, 0.10, 0.30, 0.20, 0.20];
        let lc = vec![0.10, 0.10, 0.20, 0.40, 0.20, 0.00];
        let names: Vec<String> = ["17", "29", "41"].iter().map(|s| s.to_string()).collect();
        let opts = |ns: Vec<usize>, draws| SigmaOpts { ns, mix: [0.2, 0.5, 0.3], draws, seed: 1, sigma_m_scaled: Some(35_000_000_000_000_000) };
        let v = seed_pair_sigma(&rows, &names, &[la.clone(), lb.clone(), lc.clone()], &opts(vec![10], 200)).unwrap();
        let d = &v["designs"][0];
        // dom_given = 1 in each origin: alloc = round(10 · mix) = 2, 5, 3; n = 10.
        assert_eq!(d["n_realised"], 10);
        assert_eq!(d["alloc"]["d|human_only"], 2);
        assert_eq!(d["alloc"]["d|mixed"], 5);
        assert_eq!(d["alloc"]["d|model_only"], 3);
        // Pair 17-29 by hand. Cell means: A 0.2, 0.2, 0.2 → B0 = 0.2;
        // B 0.2, 0.2, 0.2 → Bs = 0.2, r = 1. d = (L_B - L_A)/0.2:
        // human (0.5, -0.5), var1 0.5; mixed (-0.5, 0.5), 0.5; model (-1, 1), 2.
        // v = 0.04·0.5/2 + 0.25·0.5/5 + 0.09·2/3 = 0.01 + 0.025 + 0.06 = 0.095.
        let p = &d["pairs"]["17-29"];
        assert!((p["b_reference"].as_f64().unwrap() - 0.2).abs() < 1e-12);
        assert!((p["se_analytic"].as_f64().unwrap() - 0.095f64.sqrt()).abs() < 1e-12);
        assert!(p["q_point"].as_f64().unwrap().abs() < 1e-12);
        // Pair 17-41: C means 0.1, 0.3, 0.1 → Bs = 0.02 + 0.15 + 0.03 = 0.2, r = 1.
        // d = (L_C - L_A)/0.2: human (0, -1), var1 0.5; mixed (0, 1), 0.5;
        // model (-1, 0), 0.5. v = 0.04·0.5/2 + 0.25·0.5/5 + 0.09·0.5/3 = 0.01 + 0.025 + 0.015 = 0.05.
        let p2 = &d["pairs"]["17-41"];
        assert!((p2["se_analytic"].as_f64().unwrap() - 0.05f64.sqrt()).abs() < 1e-12);
        // Pair 29-41: B0 = 0.2 (B), Bs = 0.2 (C), r = 1. d = (L_C - L_B)/0.2:
        // human (-0.5, -0.5), var1 0; mixed (0.5, 0.5), 0; model (0, -1), 0.5.
        // v = 0.09·0.5/3 = 0.015.
        let p3 = &d["pairs"]["29-41"];
        assert!((p3["se_analytic"].as_f64().unwrap() - 0.015f64.sqrt()).abs() < 1e-12);
        // The analytic σ = sqrt((0.095 + 0.05 + 0.015) / 3) = sqrt(0.16/3), reported.
        assert!((d["sigma_analytic"].as_f64().unwrap() - (0.16f64 / 3.0).sqrt()).abs() < 1e-12);
        // σ is the bootstrap: the pooled per-pair bootstrap SEs, which an
        // independent replay of the replicate stream reproduces bit for bit.
        assert_eq!(d["sigma_estimator"], "stratified_bootstrap");
        let boot: Vec<f64> = ["17-29", "17-41", "29-41"].iter().map(|k| d["pairs"][*k]["se_bootstrap"].as_f64().unwrap()).collect();
        let replay = replay_bootstrap(&[la.clone(), lb.clone(), lc.clone()], &[(0, 2, 0.2), (2, 5, 0.5), (4, 3, 0.3)], 200, 1);
        assert_eq!(boot, replay);
        let sigma = (boot.iter().map(|s| s * s).sum::<f64>() / 3.0).sqrt();
        assert_eq!(d["sigma"].as_f64().unwrap(), sigma);
        assert!((sigma - (0.16f64 / 3.0).sqrt()).abs() > 1e-6, "the bootstrap differs from the analytic value here");
        assert!((d["delta"].as_f64().unwrap() - 2.0 * sigma).abs() < 1e-12);
        assert!((d["w_share_of_headroom"].as_f64().unwrap() - 2.0 * sigma).abs() < 1e-12);
        assert!((d["w_brier_at_first_seed"].as_f64().unwrap() - 2.0 * sigma * 0.2).abs() < 1e-12);
        assert_eq!(d["sigma_scaled"], to_score_scale(sigma).unwrap().to_string());
        // σ > σ_m: σ_eff = σ′; δ_eff = 2σ_eff.
        let s = to_score_scale(sigma).unwrap();
        assert!(s > 35_000_000_000_000_000);
        assert_eq!(d["sigma_eff"]["sigma_eff_scaled"], s.to_string());
        assert_eq!(d["sigma_eff"]["delta_eff_scaled"], (2 * u128::from(s)).to_string());
        assert_eq!(v["kish_ess_row_weights"], 6.0);
        assert_eq!(v["cells_below_two_documents"], 0);
        assert_eq!(v["bootstrap"]["as_pinned"], false);
        // The reward-v1 formula, reported: pairs 17-29 and 17-41 only. Its
        // weights are 0.6, 1.5, 0.9 per row (cell shares 0.2, 0.5, 0.3), B and r
        // as above, so v = (0.2·0.5 + 0.5·0.5 + 0.3·2)/10 = 0.095 and
        // (0.2·0.5 + 0.5·0.5 + 0.3·0.5)/10 = 0.05: σ = sqrt((0.095 + 0.05)/2).
        let rv1 = &d["reward_v1_cc_gates_formula"];
        assert!((rv1["pairs"]["17-29"]["se"].as_f64().unwrap() - 0.095f64.sqrt()).abs() < 1e-12);
        assert!((rv1["pairs"]["17-41"]["se"].as_f64().unwrap() - 0.05f64.sqrt()).abs() < 1e-12);
        assert!(rv1["pairs"]["29-41"].is_null());
        assert!((rv1["sigma"].as_f64().unwrap() - 0.0725f64.sqrt()).abs() < 1e-12);
        // The same seed reproduces σ; another seed moves it.
        let again = seed_pair_sigma(&rows, &names, &[la.clone(), lb.clone(), lc.clone()], &opts(vec![10], 200)).unwrap();
        assert_eq!(again["designs"][0]["sigma"], d["sigma"]);
        let other = seed_pair_sigma(&rows, &names, &[la.clone(), lb.clone(), lc.clone()], &SigmaOpts { seed: 2, ..opts(vec![10], 200) }).unwrap();
        assert_ne!(other["designs"][0]["sigma"], d["sigma"]);
        // A small σ′ never lowers σ_eff below σ_m (item 1.2).
        let tiny = seed_pair_sigma(&rows, &names, &[la.clone(), la.clone(), la.clone()], &opts(vec![1000], 10)).unwrap();
        assert_eq!(tiny["designs"][0]["sigma_scaled"], "0");
        assert_eq!(tiny["designs"][0]["sigma_eff"]["sigma_eff_scaled"], "35000000000000000");
        assert_eq!(tiny["designs"][0]["sigma_eff"]["delta_eff_scaled"], "70000000000000000");
    }

    #[test]
    fn small_cells_take_at_least_two_documents() {
        // Domain b holds 1 of 101 documents per origin: round(10 · share / 101) is 0, so its cells take 2.
        let mut rows = Vec::new();
        for o in 0..3 {
            for k in 0..100 {
                rows.push(row(&format!("a{o}-{k}"), o, "a", "g", ""));
            }
            rows.push(row(&format!("b{o}"), o, "b", "g", ""));
        }
        let (cells, om) = cells_of(&rows);
        let d = design(&cells, &om, 10, [0.2, 0.5, 0.3]).unwrap();
        let alloc: BTreeMap<String, usize> = cells.iter().zip(&d.alloc).map(|(c, &k)| (format!("{}|{}", c.domain, c.origin), k)).collect();
        assert_eq!((alloc["a|0"], alloc["a|1"], alloc["a|2"]), (2, 5, 3));
        assert_eq!((alloc["b|0"], alloc["b|1"], alloc["b|2"]), (2, 2, 2));
        assert_eq!(d.n_realised, 16);
        assert!((d.w_self.iter().sum::<f64>() - 1.0).abs() < 1e-12);
    }

    #[test]
    fn the_design_rounds_allocation_ties_half_to_even() {
        // One domain, unit weights: 18 · 0.25 = 4.5 exactly, which takes 4 (half to even), not 5.
        let rows: Vec<ARow> = (0..12).map(|i| row(&format!("r{i}"), i % 3, "d", "g", "")).collect();
        let (cells, om) = cells_of(&rows);
        let d = design(&cells, &om, 18, [0.25, 0.5, 0.25]).unwrap();
        assert_eq!(d.alloc, vec![4, 9, 4]);
        assert_eq!(d.n_realised, 17);
    }

    #[test]
    fn allocation_follows_row_weights_not_counts() {
        // Two domains with 10 documents per origin each; domain b's rows weigh 3,
        // so dom_given is 0.25 / 0.75 (by counts it would be 0.5 / 0.5).
        let mut rows = Vec::new();
        for o in 0..3 {
            for k in 0..10 {
                rows.push(row(&format!("a{o}-{k}"), o, "a", "g", ""));
                rows.push(ARow { weight: 3.0, ..row(&format!("b{o}-{k}"), o, "b", "g", "") });
            }
        }
        let (cells, om) = cells_of(&rows);
        let d = design(&cells, &om, 100, TARGET_MIX).unwrap();
        let alloc: BTreeMap<String, usize> = cells.iter().zip(&d.alloc).map(|(c, &k)| (format!("{}|{}", c.domain, c.origin), k)).collect();
        // 100 · mix · dom_given: 5 / 15, 13.75 / 41.25, 6.25 / 18.75.
        assert_eq!((alloc["a|0"], alloc["b|0"]), (5, 15));
        assert_eq!((alloc["a|1"], alloc["b|1"]), (14, 41));
        assert_eq!((alloc["a|2"], alloc["b|2"]), (6, 19));
        assert!((d.w_pop[0] - 10.0 / 120.0).abs() < 1e-15);
    }

    #[test]
    fn scope_weights_restore_the_mix_exactly() {
        // 1:2:1 rows (unweighted base rate 0.75, prior Brier 0.1875).
        let rows = layout_cue_rows(400, true);
        let s = scope_rows(&rows, (0..rows.len()).collect());
        let tot: f64 = s.w.iter().sum();
        let share = |o: usize| s.idx.iter().zip(&s.w).filter(|(i, _)| rows[**i].origin == o).map(|(_, w)| w).sum::<f64>() / tot;
        assert!((share(0) - 0.20).abs() < 1e-12 && (share(1) - 0.55).abs() < 1e-12 && (share(2) - 0.25).abs() < 1e-12);
        // The weighted prior's Brier is 0.2 · 0.8² + 0.8 · 0.2² = 0.16, up to the folds' base-rate noise.
        let r = run_adv(&rows, |f| f.layout.clone());
        for rep in &r.repeats {
            let p = rep["prior_brier"].as_f64().unwrap();
            assert!((p - 0.16).abs() < 0.005, "prior Brier {p}");
        }
    }

    #[test]
    fn the_gain_is_the_mean_over_repeats() {
        let rows = layout_cue_rows(240, true);
        let feats: Vec<Feats> = rows.iter().map(|r| features(&r.text, None)).collect();
        let s = scope_rows(&rows, (0..rows.len()).collect());
        let groups: Vec<&str> = rows.iter().map(|r| r.group.as_deref().unwrap()).collect();
        let f: Vec<Vec<f64>> = feats.iter().map(|x| x.layout.clone()).collect();
        let r = adversary(&f, &s.y, &s.w, &groups, &s.cells, &AdvConfig { draws: 20, cv_repeats: 3, ..AdvConfig::default() }).unwrap().unwrap();
        let bests: Vec<f64> = r.repeats.iter().map(|x| x["best_gain_brier"].as_f64().unwrap()).collect();
        assert_eq!(bests.len(), 3);
        assert!(r.max - r.min > 1e-9, "the repeats differ: {bests:?}");
        assert_eq!(r.gain, bests.iter().sum::<f64>() / 3.0);
        assert_eq!((r.min, r.max), (bests.iter().copied().fold(f64::INFINITY, f64::min), bests.iter().copied().fold(f64::NEG_INFINITY, f64::max)));
    }

    /// Writes rows as JSONL (block from the id prefix `c` for C).
    fn write_rows(p: &Path, rows: &[ARow]) {
        let mut s = String::new();
        for r in rows {
            s.push_str(&serde_json::to_string(&json!({"id": r.id, "text": r.text, "origin": ORIGINS[r.origin], "domain": r.domain, "source_group": r.group,
                "block": r.block.id(), "weight": r.weight, "source_kind": if r.origin == 0 { "human" } else { "local_positive" }})).unwrap());
            s.push('\n');
        }
        std::fs::write(p, s).unwrap();
    }

    fn run_gates(dir: &Path, rows: &Path, gate: Gate, b0_t: Option<f64>, b0_c: Option<f64>) -> Result<Value> {
        let a = GatesArgs { rows: rows.to_path_buf(), pool: None, b0_ref_t: b0_t, b0_ref_c: b0_c, gate, out: dir.to_path_buf(),
            cfg: AdvConfig { draws: 20, cv_repeats: 1, ..AdvConfig::default() }, probe_seed: DEFAULT_PROBE_SEED.into(), per_domain_class: 10 };
        gates(&a)?;
        Ok(serde_json::from_str::<Value>(&std::fs::read_to_string(dir.join("gates.json")).unwrap()).unwrap())
    }

    #[test]
    fn c_block_scopes_are_bounded_by_b0_ref_c() {
        let dir = tempfile::tempdir().unwrap();
        let mut rows = layout_cue_rows(240, true);
        rows.extend(layout_cue_rows(240, true).into_iter().map(|r| ARow { id: format!("c{}", r.id), group: r.group.map(|g| format!("c{g}")), block: Block::C, ..r }));
        let p = dir.path().join("rows.jsonl");
        write_rows(&p, &rows);
        // Both blocks carry a large layout gain (≈ 0.16): T passes at 0.05 · 10 = 0.5, C fails at 0.05 · 0.05.
        let v = run_gates(dir.path(), &p, Gate::G0b, Some(10.0), Some(0.05)).unwrap();
        let (t, c) = (&v["verdict"]["blocks"]["T"], &v["verdict"]["blocks"]["C"]);
        assert_eq!((t["status"].as_str(), c["status"].as_str()), (Some("pass"), Some("fail")));
        assert!((t["items"][0]["bound_brier"].as_f64().unwrap() - 0.5).abs() < 1e-15);
        assert!((c["items"][0]["bound_brier"].as_f64().unwrap() - 0.0025).abs() < 1e-15);
        assert_eq!(v["scopes"]["block:C"]["b0_ref"], 0.05);
        assert_eq!(v["verdict"]["status"], "fail");
        // Swapped bounds swap the verdicts.
        let w = run_gates(dir.path(), &p, Gate::G0b, Some(0.05), Some(10.0)).unwrap();
        assert_eq!((w["verdict"]["blocks"]["T"]["status"].as_str(), w["verdict"]["blocks"]["C"]["status"].as_str()), (Some("fail"), Some("pass")));
    }

    #[test]
    fn a_block_with_a_small_class_cell_is_still_gated() {
        // 150 humans, 150 mixed and 30 model-only rows: block T holds a 30-row cell.
        let dir = tempfile::tempdir().unwrap();
        let base = layout_cue_rows(600, true);
        let mut rows: Vec<ARow> = base.iter().filter(|r| r.origin == 0).take(150).cloned().collect();
        rows.extend(base.iter().filter(|r| r.origin == 1).take(150).cloned());
        rows.extend(base.iter().filter(|r| r.origin == 2).take(30).cloned());
        let p = dir.path().join("rows.jsonl");
        write_rows(&p, &rows);
        let v = run_gates(dir.path(), &p, Gate::G0a, None, None).unwrap();
        let t = &v["verdict"]["blocks"]["T"];
        assert_eq!(t["gated"], true);
        assert_eq!(t["items"].as_array().unwrap().len(), 2);
        assert_eq!(t["status"], "pending_b0_ref");
        assert_eq!(v["scopes"]["block:T"]["domain_class_cells"]["model_only"], 30);
        // The origin floor still decides a Tier domain: its 30-row model-only cell ungates it.
        assert_eq!(v["scopes"]["domain:T:d"]["domain_origin_cells"]["model_only"], 30);
        assert_eq!(v["scopes"]["domain:T:d"]["gated_domain_rule_met"], false);
        assert_eq!(v["verdict"]["tier_domains"]["d"]["status"], "not_gated");
        assert_eq!(v["verdict"]["status"], "pending_b0_ref");
    }

    #[test]
    fn one_row_of_a_minor_human_class_does_not_ungate_a_domain() {
        // Amendment 17 item 17.1 (the B12 landing review r2's probe B, smaller): domain d holds 60 pool humans,
        // 120 mixed and 60 model-only rows whose positives all carry a layout cue, plus one RAID, one stage-3 and
        // one Enron human. Domain e holds 2,000 clean rows, which dilute the cue in block T below the bound.
        let dir = tempfile::tempdir().unwrap();
        let d = layout_cue_rows(240, true);
        let e: Vec<ARow> = layout_cue_rows(2000, false)
            .into_iter()
            .map(|r| ARow { id: format!("e{}", r.id), domain: "e".into(), group: r.group.map(|g| format!("e{g}")), ..r })
            .collect();
        let human_text = d.iter().find(|r| r.origin == 0).unwrap().text.clone();
        let mut s = String::new();
        for r in d.iter().chain(e.iter()) {
            let kind = if r.origin == 0 { "human" } else { "local_positive" };
            let v = json!({"id": r.id, "text": r.text, "origin": ORIGINS[r.origin], "domain": r.domain, "source_group": r.group, "source_kind": kind});
            s.push_str(&serde_json::to_string(&v).unwrap());
            s.push('\n');
        }
        // One RAID, one stage-3 and one Enron human: (source kind, cohort, collection).
        let minor =
            [("external_pool", Some("raid-v1-probe"), None), ("stage3_legacy", Some("stage3_legacy_train"), None), ("human", None, Some(ENRON_COLLECTION))];
        for (k, (kind, cohort, collection)) in minor.into_iter().enumerate() {
            let v = json!({"id": format!("minor{k}"), "text": human_text, "origin": "human_only", "domain": "d", "source_group": format!("gminor{k}"),
                "source_kind": kind, "cohort_id": cohort, "collection": collection});
            s.push_str(&serde_json::to_string(&v).unwrap());
            s.push('\n');
        }
        let p = dir.path().join("rows.jsonl");
        std::fs::write(&p, &s).unwrap();
        // B0_ref,T = 0.2: the bound is 0.01.
        let v = run_gates(dir.path(), &p, Gate::G0a, Some(0.2), None).unwrap();
        let sd = &v["scopes"]["domain:T:d"];
        assert_eq!(sd["domain_class_cells"], json!({"human_enron": 1, "human_pool": 60, "human_raid": 1, "human_stage3": 1, "mixed": 120, "model_only": 60}));
        assert_eq!(sd["domain_origin_cells"], json!({"human_only": 63, "mixed": 120, "model_only": 60}));
        assert_eq!(sd["gated_domain_rule_met"], true);
        let vd = &v["verdict"]["tier_domains"]["d"];
        assert_eq!((vd["gated"].as_bool(), vd["status"].as_str()), (Some(true), Some("fail")));
        assert!(vd["items"][0]["gain_brier"].as_f64().unwrap() > 0.1, "{}", vd["items"][0]);
        assert_eq!(v["verdict"]["tier_domains"]["e"]["status"], "pass");
        // The block alone does not catch the cue: the domain's bound decides the run.
        let bt = &v["verdict"]["blocks"]["T"];
        assert_eq!(bt["status"], "pass", "{bt}");
        assert_eq!(v["verdict"]["status"], "fail");
        assert_eq!(v["verdict"]["gated_domain_rule"], GATED_DOMAIN_RULE);
    }

    #[test]
    fn g0c_needs_c_rows_and_no_gated_item_is_vacuous() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("rows.jsonl");
        write_rows(&p, &layout_cue_rows(120, true));
        let e = run_gates(dir.path(), &p, Gate::G0c, Some(0.1), Some(0.1)).unwrap_err();
        assert!(e.to_string().contains("G0c"), "{e}");
        assert_eq!(overall(&[]), "vacuous");
        // G0a on rows with a C block gates the era adversary there: without a probe pool it is refused.
        let mut rows = layout_cue_rows(120, true);
        rows.extend(layout_cue_rows(120, true).into_iter().map(|r| ARow { id: format!("c{}", r.id), group: r.group.map(|g| format!("c{g}")), block: Block::C, ..r }));
        write_rows(&p, &rows);
        let e = run_gates(dir.path(), &p, Gate::G0a, Some(0.1), Some(0.1)).unwrap_err();
        assert!(e.to_string().contains("--pool"), "{e}");
        assert!(run_gates(dir.path(), &p, Gate::G0b, Some(0.1), Some(0.1)).is_ok());
    }

    #[test]
    fn delta_eff_keeps_the_one_percent_floor() {
        // σ′ = 0 and σ_m = 0.001: δ_eff = max(0.01, 2 · 0.001) = 0.01.
        let rows: Vec<ARow> = (0..6).map(|i| row(&format!("r{i}"), i / 2, "d", &format!("g{i}"), "")).collect();
        let l = vec![0.1, 0.3, 0.2, 0.2, 0.4, 0.0];
        let names: Vec<String> = ["17", "29", "41"].iter().map(|s| s.to_string()).collect();
        let v = seed_pair_sigma(&rows, &names, &[l.clone(), l.clone(), l], &SigmaOpts { ns: vec![10], mix: TARGET_MIX, draws: 10, seed: 1, sigma_m_scaled: Some(1_000_000_000_000_000) }).unwrap();
        let e = &v["designs"][0]["sigma_eff"];
        assert_eq!((e["sigma_eff_scaled"].as_str(), e["delta_eff_scaled"].as_str()), (Some("1000000000000000"), Some("10000000000000000")));
    }

    #[test]
    fn a_named_weight_field_is_read_and_required() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("rows.jsonl");
        std::fs::write(&p, concat!(
            r#"{"id":"a","origin":"human_only","domain":"d","weight":2.0,"cohort_weight":0.5}"#, "\n",
            r#"{"id":"b","origin":"mixed","domain":"d","cohort_weight":3}"#, "\n",
        )).unwrap();
        let cw = WeightField::parse(Some("cohort_weight")).unwrap();
        let (rows, _) = read_rows_with(&p, false, &cw, |_| true).unwrap();
        assert_eq!((rows[0].weight, rows[1].weight), (0.5, 3.0));
        let (rows, _) = read_rows(&p, false).unwrap();
        assert_eq!((rows[0].weight, rows[1].weight), (2.0, 1.0));
        // A named field is required on every row, `weight` included.
        assert!(read_rows_with(&p, false, &WeightField::parse(Some("weight")).unwrap(), |_| true).is_err());
        assert!(read_rows_with(&p, false, &WeightField::parse(Some("balanced_weight")).unwrap(), |_| true).is_err());
        assert!(WeightField::parse(Some("bad field")).is_err());
        assert_eq!(cw.record()["field"], "cohort_weight");
        assert_eq!(WeightField::Default.record()["field"], "weight");
    }

    #[test]
    fn sigma_records_the_weight_field_and_allocates_by_it() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("rows.jsonl");
        let mut s = String::new();
        let mut preds = [String::new(), String::new(), String::new()];
        for i in 0..60 {
            let (o, dom) = (i % 3, if i % 2 == 0 { "a" } else { "b" });
            // Domain b weighs 3 under cohort_weight and 1 under weight.
            s.push_str(&format!("{{\"id\":\"r{i}\",\"origin\":\"{}\",\"domain\":\"{dom}\",\"source_group\":\"g{i}\",\"cohort_weight\":{}}}\n", ORIGINS[o], if dom == "b" { 3 } else { 1 }));
            for (k, pr) in preds.iter_mut().enumerate() {
                pr.push_str(&format!("{{\"id\":\"r{i}\",\"p_human\":{}}}\n", ((i * 7 + k * 3) % 10) as f64 / 10.0));
            }
        }
        std::fs::write(&p, s).unwrap();
        let mut pp = Vec::new();
        for (k, pr) in preds.iter().enumerate() {
            let f = dir.path().join(format!("p{k}.jsonl"));
            std::fs::write(&f, pr).unwrap();
            pp.push((["17", "29", "41"][k].to_string(), f));
        }
        let run = |wf: WeightField| {
            let a = SigmaArgs { rows: p.clone(), preds: pp.clone(), block: None, weight_field: wf, out: dir.path().to_path_buf(),
                opts: SigmaOpts { ns: vec![100], mix: TARGET_MIX, draws: 50, seed: SIGMA_SEED, sigma_m_scaled: None } };
            sigma(&a).unwrap();
            serde_json::from_str::<Value>(&std::fs::read_to_string(dir.path().join("sigma.json")).unwrap()).unwrap()
        };
        let v = run(WeightField::parse(Some("cohort_weight")).unwrap());
        assert_eq!(v["inputs"]["weight_field"]["field"], "cohort_weight");
        // dom_given 0.25 / 0.75: human cells 5 / 15 at n = 100.
        assert_eq!((v["designs"][0]["alloc"]["a|human_only"].as_u64(), v["designs"][0]["alloc"]["b|human_only"].as_u64()), (Some(5), Some(15)));
        // The default field (`weight`, absent here, so 1): 10 / 10.
        let u = run(WeightField::Default);
        assert_eq!(u["inputs"]["weight_field"]["field"], "weight");
        assert_eq!((u["designs"][0]["alloc"]["a|human_only"].as_u64(), u["designs"][0]["alloc"]["b|human_only"].as_u64()), (Some(10), Some(10)));
    }

    #[test]
    fn fixed_base_rate_weights_give_humans_a_fifth() {
        let base_rate = |o: &[usize]| {
            let w = fixed_base_rate_weights(o);
            o.iter().zip(&w).filter(|(x, _)| **x != 0).map(|(_, w)| w).sum::<f64>() / w.iter().sum::<f64>()
        };
        let mirrors: Vec<usize> = std::iter::repeat_n(0, 30).chain(std::iter::repeat_n(2, 70)).collect();
        assert_eq!(fixed_base_rate_shares(&mirrors), [0.2, 0.0, 0.8]);
        assert!((base_rate(&mirrors) - 0.8).abs() < 1e-12);
        let mixed: Vec<usize> = std::iter::repeat_n(0, 50).chain(std::iter::repeat_n(1, 10)).collect();
        assert_eq!(fixed_base_rate_shares(&mixed), [0.2, 0.8, 0.0]);
        assert!((base_rate(&mixed) - 0.8).abs() < 1e-12);
        // All three origins: exactly origin_weights (20/55/25).
        let all: Vec<usize> = std::iter::repeat_n(0, 10).chain(std::iter::repeat_n(1, 10)).chain(std::iter::repeat_n(2, 20)).collect();
        assert_eq!(fixed_base_rate_weights(&all), origin_weights(&all));
        // origin_weights on humans + mirrors gives 0.2 : 0.25, a base rate of 0.556.
        let ow = origin_weights(&mirrors);
        let br = mirrors.iter().zip(&ow).filter(|(x, _)| **x != 0).map(|(_, w)| w).sum::<f64>() / ow.iter().sum::<f64>();
        assert!((br - 0.25 / 0.45).abs() < 1e-12);
    }

    #[test]
    fn the_bootstrap_agrees_with_the_analytic_se() {
        // Many documents per cell: the stratified bootstrap SE of q matches
        // the analytic linearised SE (cohort-mix: within 0.1 point).
        let mut rng = Rng(3);
        let mut rows = Vec::new();
        let mut l = vec![Vec::new(), Vec::new(), Vec::new()];
        for i in 0..3000 {
            let o = i % 3;
            rows.push(row(&format!("r{i}"), o, ["a", "b"][i % 2], "g", ""));
            let base = rng.below(1000) as f64 / 1000.0 * 0.3;
            for (s, ls) in l.iter_mut().enumerate() {
                ls.push((base + rng.below(1000) as f64 / 1000.0 * 0.05 * (s + 1) as f64).min(1.0));
            }
        }
        let names: Vec<String> = ["17", "29", "41"].iter().map(|s| s.to_string()).collect();
        let v = seed_pair_sigma(&rows, &names, &l, &SigmaOpts { ns: vec![1000], mix: TARGET_MIX, draws: SIGMA_DRAWS, seed: SIGMA_SEED, sigma_m_scaled: None }).unwrap();
        assert_eq!(v["bootstrap"]["as_pinned"], true);
        let d = &v["designs"][0];
        let (b, a) = (d["sigma"].as_f64().unwrap(), d["sigma_analytic"].as_f64().unwrap());
        assert!((a - b).abs() / a < 0.06, "analytic {a} bootstrap {b}");
        // Self-weighting at the population's own mix and continuous allocation agree closely.
        let c = d["sigma_analytic_continuous_allocation"].as_f64().unwrap();
        assert!((a - c).abs() / a < 0.01, "{a} {c}");
        assert!(d["sigma_eff"].is_null());
    }

    #[test]
    fn predictions_read_involvement_as_one_minus_p_human() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("p.jsonl");
        std::fs::write(&p, concat!(r#"{"id":"a","p_human":0.25,"p_model_only":0.5,"p_mixed":0.25}"#, "\n", r#"{"id":"b","prediction":{"ai_confidence_percent":40}}"#, "\n")).unwrap();
        let m = read_predictions(&p).unwrap();
        assert_eq!((m["a"], m["b"]), (0.75, 0.4));
        std::fs::write(&p, r#"{"id":"a","p":0.3}"#).unwrap();
        assert!(read_predictions(&p).is_err());
    }
}
