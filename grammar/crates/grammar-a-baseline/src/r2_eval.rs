//! Round-2 two-block evaluator (protocol A2 §9.2, §11 and §12.6 V2, V15 and
//! V16, with amendments 1.8–1.10, 8.1 and 15; build B10). This is B10's first
//! part (BUILD-PLAN waves 4–5); [`PENDING`] lists what the second part adds.
//!
//! Numeric only, like the bound evaluator: it reads bundle metadata through
//! [`comp_eval::parse_rows`], predictions and operating points, and never
//! reads, prints or inspects a text field. The round-2 row fields it needs
//! beyond those (`block`, and a weight, unit or batch field named in the
//! configuration) are taken by key from each line's raw JSON values; nothing
//! else of the line is decoded.
//!
//! **One split per rows file** (§2.4, amendment 15 item 15.4). A population's
//! `rows.jsonl` holds its own split only. Before any line is parsed, hashed
//! or decoded, every line's `"split"` keys are read from its bytes
//! ([`line_splits`]); a line naming any other split (the old Dev-select, a
//! sealed or calibration split, anything) refuses the file, and nothing else
//! of that line is decoded.
//!
//! **The bound evaluator stays as it is** (BUILD-PLAN flag G4).
//! `src/comp_eval.rs` and `examples/a_comp_evaluate.rs` are bound by digest at
//! `ec397c2d` ([`BOUND_EVALUATOR`]) and are used read-only. Every per-seed
//! point value that both tools compute must be equal, bit for bit, or no gate
//! is evaluated (§16.2 B10, the S2 rule of `a_spans_human_gates`):
//! - each population is evaluated in process by [`comp_eval::evaluate`] (the
//!   library the bound binary calls) with the same predictions and operating
//!   points, and B10's own values are compared with its per-seed values;
//! - a population may also name the bound binary's `evaluation.json`; its
//!   recorded rows and prediction digests must be B10's inputs, and its
//!   per-seed values are compared the same way.
//!
//! The compared values ([`BOUND_POINTERS`], plus each declared operating
//! point's counts): documents, execution failures, the balanced involvement
//! Brier, the human-only balanced Brier (S2 H1), and the human and positive
//! flag counts at 0.5 and at each declared operating point. B10 computes them
//! with its own code from the same scoring rule (involvement
//! `min(p_model_only + p_mixed, 1)`; flag value `1 − p_human`; a failed or
//! missing prediction scores worst case and stays in every denominator).
//!
//! **Populations.** One `rows.jsonl` per (block, form) population, filtered to
//! its split. The gated population is the bound evaluator's primary population
//! (`population` ∈ {`primary`, `frontier`}); a producer puts every gated row
//! there (the R-form screen-fail stratum included), and other populations are
//! not read by part 1. Block T and C rows carry `block` (`T` when absent, as
//! in every v1 bundle); a mock population is block `mock`. R-form populations
//! hold the N-form humans unchanged: the human ids of an N/R pair (same block
//! and split) must be equal, and a system scored on both must give those
//! humans bit-identical losses and flags.
//!
//! **Weights** (§11, "Paired differences ... under v1.2 balanced weights
//! (cohort weights in C, W_A8 batch weights in the mock)"), each summing to 1
//! over the population's primary rows:
//! - T: the protocol balanced weights, [`comp_eval::balanced_weights`] (equal
//!   source-group mass, equal observed origin-cell mass within a group, equal
//!   document mass within a cell);
//! - C: cohort weights ([`cohort_weights`], §9.2 step 6 as read by amendment
//!   15 item 15.1): origin 20/55/25, strata at the shares of the tracked
//!   `cc_launch_shares_v5` table (pinned by digest, [`SHARES_SHA256`]; a
//!   row's stratum is its `domain`), equal mass per registered-domain group
//!   (`source_group`) within a stratum × origin cell and equal document mass
//!   within a group. An origin with no rows hands its share to the present
//!   origins in proportion, and within an origin the shares are renormalised
//!   over the strata it holds; absent origins and absent cells are recorded;
//! - mock: the W_A8 batch weights, read from the named row field (every
//!   primary row must carry a finite non-negative value), normalised.
//!
//! The human-only measure (T3c, eligibility) is the S2 H1 measure in every
//! block: balanced weights recomputed over the population's primary humans.
//!
//! **Paired differences and the bootstrap** (V2). A statistic is a sum of
//! terms `c_k · Σ_d w_d (L_A(d) − L_B(d))` over populations. Terms of one block
//! share its units: source groups in T and C, ledger units (the named unit
//! field) in the mock. Unit values are `G_s · Σ_k c_k Σ_{d∈u} w_d δ_d` over the
//! union of the block's units, so their block means sum to the statistic.
//! [`stratified_bootstrap`] resamples the blocks independently and the units
//! within each block: one xorshift64 stream (<<13 >>7 <<17, seed 20261026),
//! 1,000 draws, and within each draw the blocks in the order T, C, mock, each
//! taking as many indices as it has units; `draws[24]` and `draws[974]` of the
//! sorted draws. With one block this is exactly
//! [`crate::pangram_subsample::bootstrap_interval`] on the bound evaluator's
//! group values (tested), so a T-block comparison equals `a_comp_evaluate`'s
//! paired difference and interval bit for bit. The stream order across
//! blocks is fixed by amendment 15 item 15.2 (V2 fixes the generator, draws
//! and seed, not the order).
//!
//! **Recipe and artifact** (§11, §2.9). Recipe: per-document losses averaged
//! over the seeds (ascending) before resampling, seeds 17/29/41 against the
//! comparator's recipe seeds (17/29/41, or one seed for R0 shipped). Artifact:
//! each side's released seed, or its only seed. A reseed margin `m` is the
//! sample SD (n − 1) of its statistic over the comparator's three seeds; the
//! artifact level uses √2·m (`r2_margin_scale_v1`). A B0-scaled margin
//! (w_T/2 = σ_m · B0, B0 the reference's Brier at the gate's own weights) and a
//! fixed margin (v1.2 δ) are the same at both levels.
//!
//! **Margins first** (§2.9, S2-1, S4-2). [`run`] loads the populations and the
//! comparator systems only, computes every margin and writes the margins file
//! (or finds the byte-identical file already there), and only then reads the
//! other systems' predictions and computes any candidate-minus-comparator
//! value. The evaluation records the margins file's digest and re-reads it.
//! A system that any gate evaluates (a gate candidate) is never a comparator,
//! so no candidate output is read before the margins or supplies one; only a
//! configuration declared as §9.1 step 4's null check (Faux 29 as the
//! "candidate") may list one. The margins file records the candidate set.
//!
//! **Gates in part 1** ([`GateSpec`]): paired noninferiority and superiority
//! on any population or blend (T1a–T1c, T1e, T3c, T5d as amended by item 1.8,
//! the tie interval of §9.2 step 7); T1d's completion condition; the CP flag
//! rule (T3a; the same function serves T3g); the exact paired 0.1% test
//! `r2_human_rare_v1` (T3b); T8a (`r2_t8a_v2`, with [`t8a_branch`] and
//! amendment 8.1's realised mock shortfall). Also [`blends`]:
//! `blended_brier_r2_v2` per seed and as the three-seed mean (V15), and the
//! `r2_slice_gate_v2` arithmetic ([`z_k`], [`slice_gate_k`], amendment 15
//! item 15.3) without the slice wiring. Blends and gates are not evaluated
//! unless the per-seed values equal the bound evaluator's.
//!
//! **Splits** ([`split_policy`], amendment 15 item 15.4): a closed list.
//! Round-2 evaluation rows carry as `split` their population's amendment-10.8
//! id. Open: [`OPEN_SPLITS`]. Sealed or single-use: [`SEALED_SPLITS`]; they
//! must be named in `consume_sealed_splits`, each needs the bound binary's
//! evaluation in `release_evaluations`, and every run writes new files.
//! Refused: [`REFUSED_SPLITS`] (the old Dev-select is never read, the
//! consumed Tier C Test is not scored here, CAL is calibration only, Train is
//! fitting, and the old Dev-iter's continuity score is the bound evaluator's),
//! and any name outside the list.
//!
//! Outputs hold counts, statistics, ids of configuration items and digests,
//! never text.

use crate::comp_eval::{
    self, Missing, Obs, Options, Row, SeedInput, SystemInput, balanced_weights, bootstrap_draws,
    esc, parse_operating_points, parse_predictions, parse_rows,
};
use crate::pangram_subsample::{clopper_pearson_upper, max_flags};
use anyhow::{Context, Result, bail, ensure};
use serde::Deserialize;
use serde_json::value::RawValue;
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};
use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet};
use std::io::Write;
use std::path::{Path, PathBuf};

pub const CONFIG_SCHEMA: &str = "slop_ninja_a_r2_evaluate_config_v1";
pub const EVAL_SCHEMA: &str = "slop_ninja_a_r2_evaluation_v1";
pub const MARGINS_SCHEMA: &str = "slop_ninja_a_r2_margins_v1";
pub const PROTOCOL: &str = "PROTOCOL-A2.md §9.2, §11, §12.6 V2/V15/V16, amendments 1.8-1.10, 8.1 and 15 (build B10, part 1)";
/// The bound evaluator (BUILD-PLAN flag G4), used read-only.
pub const BOUND_EVALUATOR: &str = "grammar-a-baseline src/comp_eval.rs (blob 09898ce2971c9de518c814283f37502c0040c7b7) and examples/a_comp_evaluate.rs (blob 0806075e6d5ca5f0e7c7a4334d4143121504a051), bound at ec397c2d";
/// Git blob ids of the bound files (tested against the compiled sources).
pub const BOUND_COMP_EVAL_BLOB: &str = "09898ce2971c9de518c814283f37502c0040c7b7";
pub const BOUND_A_COMP_EVALUATE_BLOB: &str = "0806075e6d5ca5f0e7c7a4334d4143121504a051";
pub const BOOTSTRAP_METHOD: &str = "protocol source-group bootstrap (V2): xorshift64 <<13 >>7 <<17 from seed 20261026, 1,000 draws, draws[24] and draws[974] of the sorted draws; blocks resampled independently and units within blocks (source groups in T and C, ledger units in the mock); one stream, the blocks drawn in the order T, C, mock within each draw, each taking as many indices as it has units (PROTOCOL-A2 amendment 15 item 15.2)";
pub const MARGIN_RULE: &str = "PROTOCOL-A2 §2.9: reseed margin m = sample SD (n-1) of the statistic over the comparator's three seeds on the gating population, from comparator outputs only, written before any candidate-minus-comparator value; artifact level sqrt(2)*m (r2_margin_scale_v1); B0-scaled and fixed margins unscaled; no gate candidate is a comparator except in a declared null check (§9.1 step 4)";
pub const BLEND_RULE: &str = "blended_brier_r2_v2 = 1/2 B_T + 1/2 B_C, B_T = 1/2 B_T(N-form) + 1/2 B_T(R-form) (product export, protocol balanced weights), B_C = N-form (mainnet export, cohort weights) (PROTOCOL-A2 §9.2 step 6, V15)";
pub const COHORT_WEIGHT_RULE: &str = "PROTOCOL-A2 §9.2 step 6 cohort weights as read by amendment 15 item 15.1: origin 20/55/25 (human_only/mixed/model_only), strata at the tracked cc_launch_shares_v5 table (stratum = row domain), equal mass per source group within a stratum x origin cell, equal document mass within a group; absent origins hand their share to present origins in proportion, and each origin's stratum shares are renormalised over the strata it holds";
/// SHA-256 of the tracked `grammar/crates/grammar-cc-human/rules/launch-shares-v5.json`
/// (git blob cfb19d1665a5e61f7cb5073a0cb3d4ac8b77e9c1): the only share table accepted
/// (§9.2 step 6, amendment 15 item 15.1).
pub const SHARES_SHA256: &str = "7d8554d4a8e5b2c9d741adf1edfa0b2c3d065d7b3bbb556a3263eb1fc8754a62";
/// Amendment 15 item 15.4: the split classes, by the amendment-10.8 population ids
/// (`grammar_a_form::profile::R2_POPULATIONS`, tested equal) plus the v1 splits.
pub const OPEN_SPLITS: [&str; 4] = ["deviter2_t", "deviter2_c", "deviter2_mock", "span_select2"];
pub const SEALED_SPLITS: [&str; 6] = [
    "devselect2_t",
    "devselect2_c",
    "test2_t",
    "test2_c",
    "test2_mock",
    "span_test2",
];
pub const REFUSED_SPLITS: [&str; 10] = [
    "train2",
    "cal2_t",
    "cal2_c",
    "cal2_mock",
    "train",
    "train_mining",
    "dev_iter",
    "dev_select",
    "cal",
    "test",
];
pub const T8A_RULE: &str = "r2_t8a_v2";
pub const EXACT_RULE: &str = "r2_human_rare_v1";
pub const SLICE_RULE: &str = "r2_slice_gate_v2";

/// σ_m: the manifest's reward-v1 σ at n = 1,000 (V4).
pub const SIGMA_M: f64 = 0.035;
/// δ_m = 2σ_m (V4).
pub const DELTA_M: f64 = 0.070;
/// B_res: the mock reserve in batches (V6).
pub const B_RES: u64 = 58;
/// §12.3: "If s > 0.19, B_M = 58, and the shortfall is recorded in the seal" (as 19/100, exact).
pub const T8A_SHORTFALL_S: (u64, u64) = (19, 100);
/// (2.80/σ_m)² and (2.80/δ_m)² as exact integers: 2.80/0.035 = 80, 2.80/0.070 = 40 (§12.3).
pub const T8A_W_FACTOR: u64 = 6400;
pub const T8A_DELTA_FACTOR: u64 = 1600;
/// v1.2 δ (T1c, T5d).
pub const V12_DELTA: f64 = 0.010;
/// Tier C CP rule: one-sided 95% Clopper-Pearson upper bound ≤ 2.5% (T3a, T3g).
pub const CP_LEVEL: f64 = 0.95;
pub const CP_BOUND: f64 = 0.025;
/// T3b fails iff P(Bin(b + c, ½) ≥ b) < 1/20.
pub const EXACT_ALPHA_DEN: u32 = 20;
/// V1: the recipe seeds and the released seed.
pub const RECIPE_SEEDS: [u64; 3] = [17, 29, 41];
pub const RELEASED_SEED: u64 = 17;
/// Cohort origin mix in [`comp_eval::ORIGINS`] order (human_only, model_only, mixed): 20/25/55.
pub const COHORT_MIX: [f64; 3] = [0.20, 0.25, 0.55];
pub const SHARES_ID: &str = "cc_launch_shares_v5";
/// `r2_slice_gate_v2`: family-wise α and v1.2's slice bound (gate-worthiness).
pub const SLICE_ALPHA: f64 = 0.05;
pub const SLICE_BOUND: f64 = 0.030;

/// Per-seed point values compared with the bound evaluator (JSON pointers
/// into a seed object of `evaluation.json`); each declared operating point
/// adds its four counts.
pub const BOUND_POINTERS: [&str; 8] = [
    "/documents",
    "/execution_failures",
    "/balanced_involvement_brier",
    "/human_mean_brier_balanced",
    "/human_fpr_at_0_5/count",
    "/human_fpr_at_0_5/n",
    "/tpr_at_0_5/count",
    "/tpr_at_0_5/n",
];

/// What part 2 (wave 5) adds; recorded in every evaluation.
pub const PENDING: [&str; 14] = [
    "S3-1's s on the Dev-iter-2 mock (section 12.3: paired SE of r per 1,000 self-weighted documents, scaled by sqrt(ESS/1,000)) and S3-4's slice planning SD, the inputs of t8a_branch and slice_gate_k",
    "T2 slice wiring (gated slices from pipeline counts and S3-4, SE_realised, pooled T+C slices)",
    "FS, FRI and the format-v1 DiD (T5b, T5e; needs B3's N/R pairing fields)",
    "IV per H-inj group, two-sided for H-inj-L (T3f) and T3g on the B14 probe manifest slop_ninja_a_form_inject_probe_v1, line-form L twins as a stratum",
    "MWL slope (T3e) and era no-worse (T3h as amended by item 1.8)",
    "T4a/T4b slack (cross-fitted, from B11)",
    "null-check runner over item 1.10's subset with the margin-rule faults (a)-(c)",
    "selection eligibility (section 9.2 step 5), the U7 rule and selection-record-r2.json",
    "the section 11.10 decision record (recipe and artifact levels per gate), T8b reports",
    "reported R0-recal and R0-shipped companions (T1e, T5d, T3h) as named report sets",
    "sigma_eff, delta, w and epsilon from the consumed Test-2 mock at W_A8 (with B12's sigma', item 1.13; BUILD-PLAN sections 1.4 and 8)",
    "the d check on 2,000 x 1,024-document batches of the consumed Test-2 mock (BUILD-PLAN section 8)",
    "the gate floors at 0.5 on the consumed Test-2 mock, and the strict pure-human tier (BUILD-PLAN sections 1.4 and 8)",
    "the report populations' split names (old Dev-iter continuity, the CC format-v1 paired set, the CC pilot sample) by forward amendment to item 15.4's list",
];

// ------------------------------------------------------------------ config

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Deserialize)]
pub enum Block {
    T,
    C,
    #[serde(rename = "mock")]
    Mock,
}

impl Block {
    pub fn name(self) -> &'static str {
        match self {
            Block::T => "T",
            Block::C => "C",
            Block::Mock => "mock",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
pub enum Form {
    N,
    R,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub schema: String,
    #[serde(default = "released")]
    pub released_seed: u64,
    /// Every sealed or single-use split among the populations, named.
    #[serde(default)]
    pub consume_sealed_splits: Vec<String>,
    pub populations: Vec<PopulationSpec>,
    pub systems: Vec<SystemSpec>,
    /// Systems whose outputs are comparator outputs (margins read only these).
    /// No gate candidate may be one unless `null_check` is set.
    #[serde(default)]
    pub comparators: Vec<String>,
    /// §9.1 step 4's null check (Faux 29 as the "candidate" against R0, margins
    /// from Faux 17/29/41): the only configuration whose gate candidates may be
    /// comparators. Recorded in the margins file and the evaluation.
    #[serde(default)]
    pub null_check: bool,
    /// Population id to the bound binary's `evaluation.json` for that
    /// population; required for every population of a sealed split.
    #[serde(default)]
    pub release_evaluations: BTreeMap<String, PathBuf>,
    #[serde(default)]
    pub margins: Vec<MarginSpec>,
    #[serde(default)]
    pub blends: Vec<BlendSpec>,
    #[serde(default)]
    pub gates: Vec<GateSpec>,
}

fn released() -> u64 {
    RELEASED_SEED
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PopulationSpec {
    pub id: String,
    pub block: Block,
    #[serde(default)]
    pub form: Option<Form>,
    pub split: String,
    pub rows: PathBuf,
    pub weights: WeightSpec,
    /// Mock only: the row field naming each row's ledger unit.
    #[serde(default)]
    pub unit_field: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "rule", rename_all = "snake_case", deny_unknown_fields)]
pub enum WeightSpec {
    /// Block T: the protocol balanced weights.
    Balanced,
    /// Block C: cohort weights with the `cc_launch_shares_v5` table at `shares`.
    Cohort { shares: PathBuf },
    /// Mock: the W_A8 batch weights in row field `field`.
    Field { field: String },
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SystemSpec {
    pub name: String,
    pub seed: u64,
    pub population: String,
    pub predictions: PathBuf,
    /// CAL-declared operating points `{"0.01": t, "0.001": t}`.
    #[serde(default)]
    pub ops: Option<PathBuf>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Statistic {
    /// Involvement Brier at the population's gate weights.
    Brier,
    /// Human-only involvement Brier, balanced over the primary humans (S2 H1).
    HumanBrierBalanced,
}

impl Statistic {
    fn name(self) -> &'static str {
        match self {
            Statistic::Brier => "brier_gate_weights",
            Statistic::HumanBrierBalanced => "human_brier_balanced",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MarginKind {
    /// Sample SD (n − 1) over the comparator's three seeds (§2.9).
    ReseedSd,
    /// σ_m × the comparator's B0 at its released seed (w_T/2, §11.2).
    SigmaMB0,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MarginSpec {
    pub id: String,
    pub kind: MarginKind,
    pub statistic: Statistic,
    pub population: String,
    pub system: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BlendTerm {
    pub population: String,
    pub system: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BlendSpec {
    pub id: String,
    pub t_n: BlendTerm,
    pub t_r: BlendTerm,
    pub c_n: BlendTerm,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Level {
    Artifact,
    Recipe,
}

impl Level {
    fn name(self) -> &'static str {
        match self {
            Level::Artifact => "artifact",
            Level::Recipe => "recipe",
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TermSpec {
    pub population: String,
    #[serde(default = "one")]
    pub coef: f64,
    #[serde(default)]
    pub candidate: Option<String>,
    #[serde(default)]
    pub comparator: Option<String>,
    #[serde(default)]
    pub comparator_recipe: Option<String>,
}

fn one() -> f64 {
    1.0
}

/// A margin: a margins-file id, or a fixed value.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MarginRef {
    #[serde(default)]
    pub margin: Option<String>,
    #[serde(default)]
    pub value: Option<f64>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum T8aBranch {
    WHalf,
    Delta,
}

impl T8aBranch {
    fn name(self) -> &'static str {
        match self {
            T8aBranch::WHalf => "w_half",
            T8aBranch::Delta => "delta",
        }
    }
}

/// The T8a values bound in the seal (S3-1) and amendment 8.1's reserve check.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct T8aSeal {
    pub s: f64,
    pub branch: T8aBranch,
    pub b_m: u64,
    pub reserve_check: ReserveCheck,
}

/// Amendment 8.1's reserve-check record (Stage 4, B15's count-only dry run):
/// the record file, its digest (logged in `FROZEN-A2.json` as
/// `test2_mock_reserve_check_sha256` before the mock's first batch), and the
/// values it states. B_M′ is the largest feasible B ≤ B_M: B_M itself when
/// there is no shortfall, 0 when no B ≥ 1 is feasible.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReserveCheck {
    pub record: PathBuf,
    pub test2_mock_reserve_check_sha256: String,
    pub b_m: u64,
    pub b_m_prime: u64,
    pub binding_domains: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum GateSpec {
    /// Paired difference (candidate − comparator) of a statistic over terms;
    /// pass iff the 95% upper bound ≤ the margin (< with `strict`). Without
    /// a margin it is reported only.
    Paired {
        id: String,
        statistic: Statistic,
        terms: Vec<TermSpec>,
        #[serde(default)]
        candidate: Option<String>,
        #[serde(default)]
        comparator: Option<String>,
        #[serde(default)]
        comparator_recipe: Option<String>,
        levels: Vec<Level>,
        #[serde(default)]
        margin: Option<MarginRef>,
        #[serde(default)]
        strict: bool,
    },
    /// Tier C CP rule on the population's humans at a declared operating point.
    CpFlags {
        id: String,
        population: String,
        system: String,
        point: String,
        levels: Vec<Level>,
    },
    /// `r2_human_rare_v1`: exact one-sided conditional binomial test on
    /// discordant human flags, each system at its own declared threshold.
    ExactPaired {
        id: String,
        population: String,
        candidate: String,
        comparator: String,
        point: String,
        levels: Vec<Level>,
    },
    /// `r2_t8a_v2` on a mock population.
    T8a {
        id: String,
        population: String,
        candidate: String,
        comparator: String,
        #[serde(default)]
        comparator_recipe: Option<String>,
        levels: Vec<Level>,
        sealed: T8aSeal,
        /// Row field naming each row's literal batch; its distinct values must
        /// number the reserve check's B_M′ (amendment 8.1).
        batch_field: String,
    },
    /// T1d (§11.2, v1.2 condition 4): every primary document of each
    /// population returns a completed result, at the level's every seed
    /// (artifact: the released seed; recipe: 17/29/41).
    Completion {
        id: String,
        populations: Vec<String>,
        system: String,
        levels: Vec<Level>,
    },
}

impl GateSpec {
    pub fn id(&self) -> &str {
        match self {
            GateSpec::Paired { id, .. }
            | GateSpec::CpFlags { id, .. }
            | GateSpec::ExactPaired { id, .. }
            | GateSpec::T8a { id, .. }
            | GateSpec::Completion { id, .. } => id,
        }
    }

    /// The systems this gate evaluates (its candidates).
    pub fn candidates(&self) -> BTreeSet<&str> {
        let mut s = BTreeSet::new();
        match self {
            GateSpec::Paired {
                candidate, terms, ..
            } => {
                s.extend(candidate.as_deref());
                s.extend(terms.iter().filter_map(|t| t.candidate.as_deref()));
            }
            GateSpec::CpFlags { system, .. } | GateSpec::Completion { system, .. } => {
                s.insert(system.as_str());
            }
            GateSpec::ExactPaired { candidate, .. } | GateSpec::T8a { candidate, .. } => {
                s.insert(candidate.as_str());
            }
        }
        s
    }
}

pub fn parse_config(bytes: &[u8]) -> Result<Config> {
    let c: Config = serde_json::from_slice(bytes).context("B10 configuration")?;
    ensure!(
        c.schema == CONFIG_SCHEMA,
        "configuration schema {} is not {CONFIG_SCHEMA}",
        c.schema
    );
    Ok(c)
}

// ------------------------------------------------------------------ splits

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SplitPolicy {
    Open,
    Sealed,
}

/// Whether a split may be scored here, and whether it is sealed or
/// single-use: amendment 15 item 15.4's closed list.
pub fn split_policy(split: &str) -> Result<SplitPolicy> {
    if OPEN_SPLITS.contains(&split) {
        return Ok(SplitPolicy::Open);
    }
    if SEALED_SPLITS.contains(&split) {
        return Ok(SplitPolicy::Sealed);
    }
    match split {
        "dev_select" => {
            bail!("split dev_select is the old Dev-select, which is never read (PROTOCOL-A2 §2.4)")
        }
        "test" => bail!(
            "split test is the consumed Tier C Test; it is not scored by the round-2 evaluator (§2.3)"
        ),
        "cal" | "cal2_t" | "cal2_c" | "cal2_mock" => bail!(
            "split {split} is calibration only (§2.4, §9.2 step 2); the evaluator never reads it"
        ),
        "train" | "train_mining" | "train2" => {
            bail!("split {split} is fitting data; the evaluator never reads it")
        }
        "dev_iter" => bail!(
            "split dev_iter is the old Dev-iter; its continuity score is the bound evaluator's (§2.4), not a B10 population"
        ),
        _ => bail!(
            "split {split:?} is not on the closed list of B10 splits (PROTOCOL-A2 amendment 15 item 15.4)"
        ),
    }
}

/// The block a split's rows belong to (amendment 15 item 15.4): `_t` to T,
/// `_c` to C, `_mock` to the mock; the span sets hold either block, one
/// rows file per block.
pub fn split_blocks(split: &str) -> &'static [Block] {
    if split.ends_with("_mock") {
        &[Block::Mock]
    } else if split.ends_with("_t") {
        &[Block::T]
    } else if split.ends_with("_c") {
        &[Block::C]
    } else if split.starts_with("span_") {
        &[Block::T, Block::C]
    } else {
        &[]
    }
}

/// Every `"split"` key a row line names, read from its bytes without parsing
/// the line: the string after `"split"`, optional whitespace, `:` and
/// optional whitespace, up to the next quote. `None` for a value holding an
/// escape. In JSON a quote inside a string is escaped, so these are keys of
/// the line's objects.
pub fn line_splits(line: &[u8]) -> Vec<Option<&[u8]>> {
    const KEY: &[u8] = b"\"split\"";
    let mut out = vec![];
    let mut at = 0;
    while let Some(k) = line[at..].windows(KEY.len()).position(|w| w == KEY) {
        let mut i = at + k + KEY.len();
        at = at + k + 1;
        while i < line.len() && line[i].is_ascii_whitespace() {
            i += 1;
        }
        if line.get(i) != Some(&b':') {
            continue; // a string value "split", not a key
        }
        i += 1;
        while i < line.len() && line[i].is_ascii_whitespace() {
            i += 1;
        }
        if line.get(i) != Some(&b'"') {
            out.push(None);
            continue;
        }
        let start = i + 1;
        match line[start..].iter().position(|&b| b == b'"' || b == b'\\') {
            Some(e) if line[start + e] == b'"' => out.push(Some(&line[start..start + e])),
            _ => out.push(None),
        }
    }
    out
}

fn split_class(s: &[u8]) -> &'static str {
    match std::str::from_utf8(s) {
        Ok(x) if OPEN_SPLITS.contains(&x) => "an open split",
        Ok(x) if SEALED_SPLITS.contains(&x) => "a sealed or single-use split",
        Ok(x) if REFUSED_SPLITS.contains(&x) => "a refused split",
        _ => "a name outside the closed list",
    }
}

/// A population's rows file holds its own split only (amendment 15 item
/// 15.4): every non-blank line must name `split` in each of its `"split"`
/// keys, read from its bytes ([`line_splits`]) before any line is parsed,
/// decoded or hashed. The first line that does not refuses the file; nothing
/// else of it is decoded, and the refusal names its line number and the
/// split's class, never a row id.
pub fn check_rows_split(bytes: &[u8], split: &str) -> Result<usize> {
    let mut lines = 0;
    for (i, line) in bytes.split(|&b| b == b'\n').enumerate() {
        if line.iter().all(|b| b.is_ascii_whitespace()) {
            continue;
        }
        let names = line_splits(line);
        ensure!(
            !names.is_empty(),
            "rows.jsonl line {} names no split in its bytes; refused before any parse",
            i + 1
        );
        for n in names {
            match n {
                Some(n) if n == split.as_bytes() => {}
                Some(n) => bail!(
                    "rows.jsonl line {} names {} ({}), not this population's split {split}: a B10 rows file holds one split (PROTOCOL-A2 amendment 15 item 15.4); refused from the line's split bytes, before any parse",
                    i + 1,
                    split_class(n),
                    String::from_utf8_lossy(n)
                ),
                None => bail!(
                    "rows.jsonl line {}: a split value that cannot be read from the line's bytes; refused before any parse",
                    i + 1
                ),
            }
        }
        lines += 1;
    }
    Ok(lines)
}

fn check_splits(cfg: &Config) -> Result<BTreeSet<String>> {
    let mut sealed = BTreeSet::new();
    for p in &cfg.populations {
        let policy = split_policy(&p.split).with_context(|| format!("population {}", p.id))?;
        ensure!(
            split_blocks(&p.split).contains(&p.block),
            "population {}: split {} does not hold block {} rows (amendment 15 item 15.4)",
            p.id,
            p.split,
            p.block.name()
        );
        if policy == SplitPolicy::Sealed {
            sealed.insert(p.split.clone());
        }
    }
    let named: BTreeSet<String> = cfg.consume_sealed_splits.iter().cloned().collect();
    ensure!(
        named.len() == cfg.consume_sealed_splits.len(),
        "consume_sealed_splits names a split twice"
    );
    ensure!(
        named == sealed,
        "consume_sealed_splits {named:?} must name exactly the sealed or single-use splits {sealed:?}"
    );
    for p in cfg.populations.iter().filter(|p| sealed.contains(&p.split)) {
        ensure!(
            cfg.release_evaluations.contains_key(&p.id),
            "population {} is of sealed split {}: its per-seed values must be compared with the bound binary's evaluation.json, named in release_evaluations (S2 rule)",
            p.id,
            p.split
        );
    }
    Ok(sealed)
}

// ------------------------------------------------------------------ files

pub fn sha256_hex(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

fn read(p: &Path) -> Result<Vec<u8>> {
    std::fs::read(p).with_context(|| format!("read {}", p.display()))
}

/// Write `bytes` to a new file (mode 600 on Unix), atomically.
pub fn write_new(p: &Path, bytes: &[u8]) -> Result<()> {
    ensure!(
        !p.exists(),
        "{} exists: a new output is required",
        p.display()
    );
    let tmp = p.with_extension("tmp-b10");
    {
        let mut o = std::fs::OpenOptions::new();
        o.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            o.mode(0o600);
        }
        let mut f = o
            .open(&tmp)
            .with_context(|| format!("create {}", tmp.display()))?;
        f.write_all(bytes)?;
        f.sync_all()?;
    }
    std::fs::rename(&tmp, p)?;
    Ok(())
}

fn pretty(v: &Value) -> Result<Vec<u8>> {
    let mut b = serde_json::to_vec_pretty(v)?;
    b.push(b'\n');
    Ok(b)
}

// ------------------------------------------------------------------ populations

/// One loaded (block, form) population.
pub struct Population {
    pub id: String,
    pub block: Block,
    pub form: Option<Form>,
    pub split: String,
    pub rows_path: PathBuf,
    pub rows_sha256: String,
    /// The split's rows, in file order.
    pub rows: Vec<Row>,
    /// Row ids of the split.
    pub ids: BTreeSet<String>,
    /// Indices into `rows` of the primary population (the gated rows).
    pub primary: Vec<usize>,
    /// Balanced weights over `primary` (the bound evaluator's).
    pub balanced: Vec<f64>,
    /// Gate weights over `primary` (sum 1).
    pub weights: Vec<f64>,
    pub weight_record: Value,
    /// Bootstrap unit per primary position (source group, or ledger unit).
    pub units: Vec<String>,
    /// Positions (into `primary`) of the humans.
    pub humans: Vec<usize>,
    /// Balanced weights over the primary humans (S2 H1).
    pub human_weights: Vec<f64>,
    /// Extra named fields per primary position (batch fields).
    pub extra: BTreeMap<String, Vec<String>>,
}

fn scalar_key(v: &Value, what: &str, id: &str) -> Result<String> {
    match v {
        Value::String(s) if !s.is_empty() => Ok(s.clone()),
        Value::Number(n) if n.is_u64() || n.is_i64() => Ok(n.to_string()),
        _ => bail!("row {id}: {what} must be a non-empty string or an integer"),
    }
}

/// The named raw fields of the rows, by id (nothing else is decoded). The
/// file holds one split ([`check_rows_split`] ran first).
fn raw_fields(
    text: &str,
    split: &str,
    wanted: &BTreeSet<String>,
) -> Result<BTreeMap<String, BTreeMap<String, Value>>> {
    let mut out = BTreeMap::new();
    for (i, line) in text.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let m: BTreeMap<String, &RawValue> =
            serde_json::from_str(line).with_context(|| format!("rows.jsonl line {}", i + 1))?;
        let s: String = serde_json::from_str(m.get("split").context("row without split")?.get())?;
        ensure!(s == split, "rows.jsonl line {}: split {s}", i + 1);
        let id: String = serde_json::from_str(m.get("id").context("row without id")?.get())?;
        let mut f = BTreeMap::new();
        for k in wanted {
            if let Some(v) = m.get(k) {
                f.insert(k.clone(), serde_json::from_str::<Value>(v.get())?);
            }
        }
        out.insert(id, f);
    }
    Ok(out)
}

/// A `cc_launch_shares_v5` table (percent of human parents per stratum).
#[derive(Clone, Debug)]
pub struct ShareTable {
    pub id: String,
    pub shares: BTreeMap<String, f64>,
    pub press_release: f64,
    pub list_mail: f64,
    pub sha256: String,
}

impl ShareTable {
    /// The tracked `cc_launch_shares_v5` table, refused unless its bytes hash to [`SHARES_SHA256`].
    pub fn parse(bytes: &[u8]) -> Result<ShareTable> {
        let sha = sha256_hex(bytes);
        ensure!(
            sha == SHARES_SHA256,
            "share table hashes to {sha}, not the tracked cc_launch_shares_v5 table {SHARES_SHA256} (PROTOCOL-A2 §9.2 step 6, amendment 15 item 15.1)"
        );
        let v: Value = serde_json::from_slice(bytes).context("share table")?;
        let id = v["id"].as_str().context("share table id")?.to_string();
        ensure!(
            id == SHARES_ID,
            "share table {id} is not {SHARES_ID} (PROTOCOL-A2 §9.2 step 6: strata at v5 shares)"
        );
        let mut shares = BTreeMap::new();
        for (k, x) in v["shares"].as_object().context("share table shares")? {
            let x = x.as_f64().context("share")?;
            ensure!(x.is_finite() && x >= 0.0, "share table: bad share for {k}");
            shares.insert(k.clone(), x);
        }
        let total: f64 = shares.values().sum();
        ensure!(
            (total - 100.0).abs() < 1e-6,
            "share table shares sum to {total}"
        );
        let press_release = v["email_split"]["press_release"]
            .as_f64()
            .context("press_release share")?;
        let list_mail = v["email_split"]["list_mail"]
            .as_f64()
            .context("list_mail share")?;
        let email = *shares.get("email").context("share table without email")?;
        ensure!(
            (press_release + list_mail - email).abs() < 1e-9,
            "share table: email split does not sum to email"
        );
        Ok(ShareTable {
            id,
            shares,
            press_release,
            list_mail,
            sha256: sha,
        })
    }

    /// Percent share of a stratum (a domain, `press_release` or `list_mail`).
    pub fn stratum(&self, s: &str) -> Option<f64> {
        match s {
            "press_release" => Some(self.press_release),
            "list_mail" => Some(self.list_mail),
            _ => self.shares.get(s).copied(),
        }
    }
}

/// Kish effective sample size of a weight vector.
pub fn kish_ess(w: &[f64]) -> f64 {
    let s: f64 = w.iter().sum();
    let q: f64 = w.iter().map(|x| x * x).sum();
    if q > 0.0 { s * s / q } else { 0.0 }
}

/// Cohort weights over `idx` (sum 1; see the module documentation).
pub fn cohort_weights(
    rows: &[Row],
    idx: &[usize],
    table: &ShareTable,
) -> Result<(Vec<f64>, Value)> {
    ensure!(!idx.is_empty(), "no rows to weight");
    let email = idx.iter().any(|&i| rows[i].domain == "email");
    let split = idx
        .iter()
        .any(|&i| rows[i].domain == "press_release" || rows[i].domain == "list_mail");
    ensure!(
        !(email && split),
        "C rows use both email and its press_release/list_mail split"
    );
    for &i in idx {
        ensure!(
            table.stratum(&rows[i].domain).is_some(),
            "row {}: stratum {} is not in {SHARES_ID}",
            rows[i].id,
            rows[i].domain
        );
    }
    // cell (origin, stratum) -> group -> documents
    let mut cells: BTreeMap<(usize, &str), BTreeMap<&str, usize>> = BTreeMap::new();
    for &i in idx {
        let r = &rows[i];
        *cells
            .entry((r.origin, r.domain.as_str()))
            .or_default()
            .entry(r.group.as_str())
            .or_default() += 1;
    }
    let present: BTreeSet<usize> = cells.keys().map(|k| k.0).collect();
    let mix_total: f64 = present.iter().map(|&o| COHORT_MIX[o]).sum();
    let mut stratum_total = [0.0f64; 3];
    for &(o, s) in cells.keys() {
        stratum_total[o] += table.stratum(s).unwrap();
    }
    for &o in &present {
        ensure!(
            stratum_total[o] > 0.0,
            "origin {} holds only zero-share strata",
            comp_eval::ORIGINS[o]
        );
    }
    let mass = |o: usize, s: &str| {
        (COHORT_MIX[o] / mix_total) * (table.stratum(s).unwrap() / stratum_total[o])
    };
    let w: Vec<f64> = idx
        .iter()
        .map(|&i| {
            let r = &rows[i];
            let c = &cells[&(r.origin, r.domain.as_str())];
            mass(r.origin, &r.domain) / c.len() as f64 / c[r.group.as_str()] as f64
        })
        .collect();
    let total: f64 = w.iter().sum();
    ensure!((total - 1.0).abs() < 1e-9, "cohort weights sum to {total}");
    let mut by_cell = Map::new();
    for ((o, s), g) in &cells {
        by_cell.insert(
            format!("{}|{s}", comp_eval::ORIGINS[*o]),
            json!({"mass": mass(*o, s), "source_groups": g.len(), "documents": g.values().sum::<usize>()}),
        );
    }
    let absent: Vec<&str> = (0..3)
        .filter(|o| !present.contains(o))
        .map(|o| comp_eval::ORIGINS[o])
        .collect();
    // Cells of a present origin and a stratum some origin holds, with no rows.
    let strata: BTreeSet<&str> = cells.keys().map(|k| k.1).collect();
    let absent_cells: Vec<String> = present
        .iter()
        .flat_map(|&o| strata.iter().map(move |&s| (o, s)))
        .filter(|k| !cells.contains_key(k))
        .map(|(o, s)| format!("{}|{s}", comp_eval::ORIGINS[o]))
        .collect();
    let mut origin_mass = Map::new();
    for &o in &present {
        origin_mass.insert(
            comp_eval::ORIGINS[o].into(),
            json!(COHORT_MIX[o] / mix_total),
        );
    }
    Ok((
        w,
        json!({"rule": COHORT_WEIGHT_RULE, "share_table": table.id, "share_table_sha256": table.sha256,
               "origin_mass": origin_mass, "absent_origins": absent, "absent_cells": absent_cells, "cells": by_cell}),
    ))
}

/// Normalise non-negative finite weights to sum 1.
pub fn field_weights(raw: &[f64]) -> Result<Vec<f64>> {
    for &x in raw {
        ensure!(
            x.is_finite() && x >= 0.0,
            "weight {x} is not finite and non-negative"
        );
    }
    let t: f64 = raw.iter().sum();
    ensure!(t > 0.0, "weights sum to 0");
    Ok(raw.iter().map(|x| x / t).collect())
}

fn load_population(spec: &PopulationSpec, extra_fields: &BTreeSet<String>) -> Result<Population> {
    split_policy(&spec.split)?;
    let bytes = read(&spec.rows)?;
    // Every line's split from its bytes, before any line is parsed or hashed.
    check_rows_split(&bytes, &spec.split)
        .with_context(|| format!("population {} ({})", spec.id, spec.rows.display()))?;
    let rows_sha256 = sha256_hex(&bytes);
    let text = std::str::from_utf8(&bytes).context("rows.jsonl is not UTF-8")?;
    let rows: Vec<Row> = parse_rows(text)?;
    ensure!(
        !rows.is_empty(),
        "population {}: no rows of split {}",
        spec.id,
        spec.split
    );
    for r in &rows {
        ensure!(
            r.split == spec.split,
            "population {}: row {} is in split {}",
            spec.id,
            r.id,
            r.split
        );
    }
    let mut wanted: BTreeSet<String> = extra_fields.clone();
    wanted.insert("block".into());
    if let WeightSpec::Field { field } = &spec.weights {
        wanted.insert(field.clone());
    }
    if let Some(u) = &spec.unit_field {
        wanted.insert(u.clone());
    }
    let raw = raw_fields(text, &spec.split, &wanted)?;
    match (spec.block, &spec.weights, &spec.unit_field, spec.form) {
        (Block::T, WeightSpec::Balanced, None, _)
        | (Block::C, WeightSpec::Cohort { .. }, None, _) => {}
        (Block::Mock, WeightSpec::Field { .. }, Some(_), None) => {}
        _ => bail!(
            "population {}: block T takes balanced weights, block C cohort weights, and a mock field weights with a unit field and no form",
            spec.id
        ),
    }
    if spec.block != Block::Mock {
        for r in &rows {
            let b = match raw[&r.id].get("block") {
                None | Some(Value::Null) => "T".to_string(),
                Some(Value::String(s)) => s.clone(),
                Some(_) => bail!("row {}: block must be a string", r.id),
            };
            ensure!(
                b == spec.block.name(),
                "row {} is in block {b}, not population {}'s block {}",
                r.id,
                spec.id,
                spec.block.name()
            );
        }
    }
    let primary: Vec<usize> = (0..rows.len()).filter(|&i| rows[i].primary).collect();
    ensure!(
        !primary.is_empty(),
        "population {}: no primary rows",
        spec.id
    );
    let balanced = balanced_weights(&rows, &primary);
    let (weights, weight_record) = match &spec.weights {
        WeightSpec::Balanced => (
            balanced.clone(),
            json!({"rule": "protocol balanced weights (comp_eval::balanced_weights over the primary rows)"}),
        ),
        WeightSpec::Cohort { shares } => {
            cohort_weights(&rows, &primary, &ShareTable::parse(&read(shares)?)?)?
        }
        WeightSpec::Field { field } => {
            let mut v = Vec::with_capacity(primary.len());
            for &i in &primary {
                let x = raw[&rows[i].id]
                    .get(field)
                    .and_then(Value::as_f64)
                    .with_context(|| {
                        format!("row {} has no numeric weight field {field}", rows[i].id)
                    })?;
                v.push(x);
            }
            (
                field_weights(&v)
                    .with_context(|| format!("population {} weight field {field}", spec.id))?,
                json!({"rule": "W_A8 batch weights from the named row field, normalised", "field": field}),
            )
        }
    };
    let units: Vec<String> = match &spec.unit_field {
        None => primary.iter().map(|&i| rows[i].group.clone()).collect(),
        Some(u) => primary
            .iter()
            .map(|&i| {
                let id = &rows[i].id;
                scalar_key(
                    raw[id]
                        .get(u)
                        .with_context(|| format!("row {id} has no unit field {u}"))?,
                    u,
                    id,
                )
            })
            .collect::<Result<_>>()?,
    };
    let humans: Vec<usize> = (0..primary.len())
        .filter(|&p| rows[primary[p]].is_human())
        .collect();
    let hrows: Vec<usize> = humans.iter().map(|&p| primary[p]).collect();
    let human_weights = if hrows.is_empty() {
        vec![]
    } else {
        balanced_weights(&rows, &hrows)
    };
    let mut extra = BTreeMap::new();
    for f in extra_fields {
        let v = primary
            .iter()
            .map(|&i| {
                let id = &rows[i].id;
                scalar_key(
                    raw[id]
                        .get(f)
                        .with_context(|| format!("row {id} has no field {f}"))?,
                    f,
                    id,
                )
            })
            .collect::<Result<Vec<_>>>()?;
        extra.insert(f.clone(), v);
    }
    let ids = rows.iter().map(|r| r.id.clone()).collect();
    Ok(Population {
        id: spec.id.clone(),
        block: spec.block,
        form: spec.form,
        split: spec.split.clone(),
        rows_path: spec.rows.clone(),
        rows_sha256,
        rows,
        ids,
        primary,
        balanced,
        weights,
        weight_record,
        units,
        humans,
        human_weights,
        extra,
    })
}

impl Population {
    fn summary(&self) -> Value {
        let units: BTreeSet<&str> = self.units.iter().map(String::as_str).collect();
        let mut rec = self.weight_record.clone();
        rec["kish_ess"] = json!(kish_ess(&self.weights));
        json!({
            "block": self.block.name(),
            "form": self.form.map(|f| match f { Form::N => "N", Form::R => "R" }),
            "split": self.split,
            "rows": self.rows_path,
            "rows_jsonl_sha256": self.rows_sha256,
            "split_documents": self.rows.len(),
            "primary_documents": self.primary.len(),
            "humans": self.humans.len(),
            "units": units.len(),
            "unit": if self.block == Block::Mock { "ledger unit (named field)" } else { "source group" },
            "weights": rec,
            "human_weights_kish_ess": kish_ess(&self.human_weights),
        })
    }

    fn human_ids(&self) -> BTreeSet<&str> {
        self.humans
            .iter()
            .map(|&p| self.rows[self.primary[p]].id.as_str())
            .collect()
    }
}

// ------------------------------------------------------------------ scoring

/// One system seed on one population, aligned to its primary rows.
pub struct SeedRun {
    pub seed: u64,
    pub loss: Vec<f64>,
    pub flag: Vec<f64>,
    pub failed: usize,
    pub missing: usize,
    pub ops: Option<BTreeMap<&'static str, f64>>,
    pub predictions: BTreeMap<String, Obs>,
    pub inputs: Value,
    pub pred_sha256: String,
    pub ops_sha256: Option<String>,
}

/// The bound scoring rule for one document: (loss, flag value, failed).
pub fn score_doc(r: &Row, o: Option<&Obs>) -> (f64, f64, bool) {
    match o {
        Some(Obs::Ok(p)) => {
            let inv = (p.p_model_only + p.p_mixed).min(1.0);
            (
                (inv - r.y).powi(2),
                (1.0 - p.p_human).clamp(0.0, 1.0),
                false,
            )
        }
        _ => {
            let inv = 1.0 - r.y;
            ((inv - r.y).powi(2), 1.0 - r.y, true)
        }
    }
}

fn load_run(pop: &Population, spec: &SystemSpec) -> Result<SeedRun> {
    let bytes = read(&spec.predictions)?;
    let pred_sha256 = sha256_hex(&bytes);
    let predictions =
        parse_predictions(std::str::from_utf8(&bytes).context("predictions are not UTF-8")?)
            .with_context(|| format!("predictions {}", spec.predictions.display()))?;
    for id in predictions.keys() {
        ensure!(
            pop.ids.contains(id),
            "{} seed {}: prediction {id} is not a document of population {} (split {})",
            spec.name,
            spec.seed,
            pop.id,
            pop.split
        );
    }
    let (ops, ops_sha256) = match &spec.ops {
        Some(p) => {
            let b = read(p)?;
            (
                Some(parse_operating_points(&serde_json::from_slice(&b)?)?),
                Some(sha256_hex(&b)),
            )
        }
        None => (None, None),
    };
    let (mut loss, mut flag) = (
        Vec::with_capacity(pop.primary.len()),
        Vec::with_capacity(pop.primary.len()),
    );
    let (mut failed, mut missing) = (0, 0);
    for &i in &pop.primary {
        let r = &pop.rows[i];
        let o = predictions.get(&r.id);
        missing += o.is_none() as usize;
        let (l, f, x) = score_doc(r, o);
        loss.push(l);
        flag.push(f);
        failed += x as usize;
    }
    Ok(SeedRun {
        seed: spec.seed,
        loss,
        flag,
        failed,
        missing,
        ops,
        predictions,
        inputs: json!({"predictions": spec.predictions, "predictions_sha256": pred_sha256, "ops": spec.ops, "ops_sha256": ops_sha256}),
        pred_sha256,
        ops_sha256,
    })
}

/// Population id → system → seed → run.
pub type Book = BTreeMap<String, BTreeMap<String, BTreeMap<u64, SeedRun>>>;

fn wmean(w: &[f64], x: &[f64]) -> f64 {
    w.iter().zip(x).map(|(a, b)| a * b).sum()
}

fn rate(c: usize, n: usize) -> Value {
    json!({"count": c, "n": n})
}

/// B10's own per-seed values; `bound` holds the values compared with the bound evaluator.
pub fn seed_values(pop: &Population, run: &SeedRun) -> Value {
    let hloss: Vec<f64> = pop.humans.iter().map(|&p| run.loss[p]).collect();
    let is_h: Vec<bool> = (0..pop.primary.len())
        .map(|p| pop.rows[pop.primary[p]].is_human())
        .collect();
    let count = |t: f64, human: bool| {
        (0..is_h.len())
            .filter(|&p| is_h[p] == human && run.flag[p] >= t)
            .count()
    };
    let nh = pop.humans.len();
    let np = pop.primary.len() - nh;
    let mut ops = Map::new();
    if let Some(m) = &run.ops {
        for (k, &t) in m {
            ops.insert(
                (*k).into(),
                json!({"human_fpr": rate(count(t, true), nh), "tpr": rate(count(t, false), np)}),
            );
        }
    }
    let bound = json!({
        "documents": pop.primary.len(),
        "execution_failures": run.failed,
        "balanced_involvement_brier": wmean(&pop.balanced, &run.loss),
        "human_mean_brier_balanced": if nh == 0 { Value::Null } else { json!(wmean(&pop.human_weights, &hloss)) },
        "human_fpr_at_0_5": rate(count(0.5, true), nh),
        "tpr_at_0_5": rate(count(0.5, false), np),
        "operating_points": ops,
    });
    json!({
        "bound": bound,
        "gate_weight_brier": wmean(&pop.weights, &run.loss),
        "missing_predictions": run.missing,
        "inputs": run.inputs,
    })
}

fn bound_pointers(run: &SeedRun) -> Vec<String> {
    let mut v: Vec<String> = BOUND_POINTERS.iter().map(|s| s.to_string()).collect();
    if let Some(m) = &run.ops {
        for k in m.keys() {
            for p in ["human_fpr/count", "human_fpr/n", "tpr/count", "tpr/n"] {
                v.push(format!("/operating_points/{}/{p}", esc(k)));
            }
        }
    }
    v
}

/// Exact equality of two JSON scalars: integers by value, floats by bits.
fn same(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Null, Value::Null) => true,
        (Value::Number(x), Value::Number(y)) => match (x.as_u64(), y.as_u64()) {
            (Some(p), Some(q)) => p == q,
            (None, None) => x.as_f64().map(f64::to_bits) == y.as_f64().map(f64::to_bits),
            _ => false,
        },
        _ => false,
    }
}

// ------------------------------------------------------------------ equality (S2 rule)

fn compare_seed(
    out: &mut Vec<Value>,
    source: &str,
    pop: &str,
    name: &str,
    run: &SeedRun,
    mine: &Value,
    theirs: Option<&Value>,
) -> usize {
    let mut n = 0;
    for ptr in bound_pointers(run) {
        n += 1;
        let a = mine.pointer(&ptr).cloned().unwrap_or(Value::Null);
        let b = theirs.and_then(|t| t.pointer(&ptr)).cloned();
        let ok = b.as_ref().is_some_and(|b| same(&a, b));
        if !ok {
            out.push(json!({"source": source, "population": pop, "system": name, "seed": run.seed, "value": ptr, "b10": a, "bound": b}));
        }
    }
    n
}

/// Compare B10's per-seed values with the bound evaluator's on every population.
fn equality(
    cfg: &Config,
    pops: &BTreeMap<String, Population>,
    book: &Book,
    values: &Value,
) -> Result<Value> {
    let mut mismatches = vec![];
    let mut errors = vec![];
    let mut checked = 0usize;
    let mut external = Map::new();
    let opts = Options {
        released_seed: cfg.released_seed,
        ..Options::default()
    };
    for (pid, pop) in pops {
        let Some(systems) = book.get(pid) else {
            continue;
        };
        let inputs: Vec<SystemInput> = systems
            .iter()
            .map(|(name, seeds)| SystemInput {
                name: name.clone(),
                seeds: seeds
                    .values()
                    .map(|r| SeedInput {
                        seed: r.seed,
                        predictions: r.predictions.clone(),
                        operating_points: r.ops.clone(),
                    })
                    .collect(),
                subsample: None,
                missing: Missing::Worst,
            })
            .collect();
        match comp_eval::evaluate(&pop.rows, &inputs, &[], &opts) {
            Err(e) => errors.push(json!({"population": pid, "source": "comp_eval::evaluate", "error": format!("{e:#}")})),
            Ok(e) => {
                for (name, seeds) in systems {
                    for run in seeds.values() {
                        let mine = &values[pid][name][run.seed.to_string()]["bound"];
                        let theirs = e.pointer(&format!("/systems/{}/seeds/{}", esc(name), run.seed));
                        checked += compare_seed(&mut mismatches, "in_process", pid, name, run, mine, theirs);
                    }
                }
            }
        }
        if let Some(path) = cfg.release_evaluations.get(pid) {
            let b = read(path)?;
            let ev: Value =
                serde_json::from_slice(&b).with_context(|| format!("{}", path.display()))?;
            let mut problems = vec![];
            if ev["schema"] != comp_eval::EVAL_SCHEMA {
                problems.push("schema".to_string());
            }
            if ev["split"] != pop.split.as_str() {
                problems.push("split".into());
            }
            if ev["inputs"]["rows_jsonl_sha256"] != pop.rows_sha256.as_str() {
                problems.push("rows_jsonl_sha256".into());
            }
            for (name, seeds) in systems {
                for run in seeds.values() {
                    let f = &ev["inputs"]["files"];
                    if f[format!("pred:{name}:{}", run.seed)]["sha256"] != run.pred_sha256.as_str()
                    {
                        problems.push(format!("pred:{name}:{}", run.seed));
                    }
                    let o = &f[format!("ops:{name}:{}", run.seed)]["sha256"];
                    let ops_differ = match run.ops_sha256.as_deref() {
                        None => !o.is_null(),
                        Some(s) => o.as_str() != Some(s),
                    };
                    if ops_differ {
                        problems.push(format!("ops:{name}:{}", run.seed));
                    }
                    let mine = &values[pid][name][run.seed.to_string()]["bound"];
                    let theirs = ev.pointer(&format!("/systems/{}/seeds/{}", esc(name), run.seed));
                    checked += compare_seed(
                        &mut mismatches,
                        "release_evaluation",
                        pid,
                        name,
                        run,
                        mine,
                        theirs,
                    );
                }
            }
            if !problems.is_empty() {
                errors.push(json!({"population": pid, "source": "release_evaluation", "error": "inputs differ", "fields": problems}));
            }
            external.insert(pid.clone(), json!({"path": path, "sha256": sha256_hex(&b)}));
        }
    }
    for pid in cfg.release_evaluations.keys() {
        ensure!(
            pops.contains_key(pid),
            "release_evaluations names unknown population {pid}"
        );
    }
    let pass = mismatches.is_empty() && errors.is_empty() && checked > 0;
    Ok(json!({
        "rule": "per-seed point values equal the bound evaluator's bit for bit, or no gate is evaluated (PROTOCOL-A2 §16.2 B10; S2 rule)",
        "bound_evaluator": BOUND_EVALUATOR,
        "values_checked": checked,
        "release_evaluations": external,
        "mismatches": mismatches,
        "errors": errors,
        "pass": pass,
    }))
}

// ------------------------------------------------------------------ bootstrap

/// The protocol bootstrap over independently resampled blocks (see the
/// module documentation). Returns the 1,000 sorted draws of
/// `Σ_s mean(resampled units of block s)`; `None` when any block has fewer
/// than two units.
pub fn stratified_bootstrap(blocks: &[&[f64]]) -> Option<Vec<f64>> {
    if blocks.is_empty() || blocks.iter().any(|b| b.len() < 2) {
        return None;
    }
    let mut seed = 20261026u64;
    let mut draws = Vec::with_capacity(1000);
    for _ in 0..1000 {
        let mut stat: Option<f64> = None;
        for b in blocks {
            let mut total = 0.0;
            for _ in 0..b.len() {
                seed ^= seed << 13;
                seed ^= seed >> 7;
                seed ^= seed << 17;
                total += b[seed as usize % b.len()];
            }
            let m = total / b.len() as f64;
            stat = Some(stat.map_or(m, |x| x + m));
        }
        draws.push(stat.unwrap());
    }
    draws.sort_by(f64::total_cmp);
    Some(draws)
}

/// Sample SD (n − 1).
pub fn sample_sd(v: &[f64]) -> Option<f64> {
    if v.len() < 2 {
        return None;
    }
    let m = v.iter().sum::<f64>() / v.len() as f64;
    Some((v.iter().map(|x| (x - m).powi(2)).sum::<f64>() / (v.len() - 1) as f64).sqrt())
}

/// One term of a composite paired statistic, aligned to the documents it covers.
pub struct Term<'a> {
    pub block: Block,
    pub units: Vec<&'a str>,
    pub weights: Vec<f64>,
    pub delta: Vec<f64>,
    pub coef: f64,
}

/// Unit values per block (block order, units in key order) and the point estimate.
pub fn composite_units(terms: &[Term]) -> (f64, Vec<(Block, Vec<f64>)>) {
    let blocks: BTreeSet<Block> = terms.iter().map(|t| t.block).collect();
    let mut out = vec![];
    let mut point: Option<f64> = None;
    for b in blocks {
        let mut m: BTreeMap<&str, f64> = BTreeMap::new();
        for t in terms.iter().filter(|t| t.block == b) {
            for u in &t.units {
                m.entry(u).or_default();
            }
        }
        let g = m.len() as f64;
        for t in terms.iter().filter(|t| t.block == b) {
            for k in 0..t.units.len() {
                *m.get_mut(t.units[k]).unwrap() += g * t.weights[k] * t.delta[k] * t.coef;
            }
        }
        let v: Vec<f64> = m.into_values().collect();
        let p = v.iter().sum::<f64>() / v.len() as f64;
        point = Some(point.map_or(p, |x| x + p));
        out.push((b, v));
    }
    (point.unwrap_or(f64::NAN), out)
}

// ------------------------------------------------------------------ exact arithmetic

/// Minimal unsigned big integer (little-endian u32 limbs, no trailing zeros).
#[derive(Clone, Debug, PartialEq, Eq)]
struct Big(Vec<u32>);

impl Big {
    fn from_u64(mut x: u64) -> Big {
        let mut v = vec![];
        while x > 0 {
            v.push(x as u32);
            x >>= 32;
        }
        Big(v)
    }
    fn trim(&mut self) {
        while self.0.last() == Some(&0) {
            self.0.pop();
        }
    }
    fn mul_small(&mut self, m: u32) {
        let mut carry = 0u64;
        for d in self.0.iter_mut() {
            let t = *d as u64 * m as u64 + carry;
            *d = t as u32;
            carry = t >> 32;
        }
        if carry > 0 {
            self.0.push(carry as u32);
        }
        self.trim();
    }
    /// Divide in place; returns the remainder.
    fn div_small(&mut self, d: u32) -> u32 {
        let mut rem = 0u64;
        for x in self.0.iter_mut().rev() {
            let cur = (rem << 32) | *x as u64;
            *x = (cur / d as u64) as u32;
            rem = cur % d as u64;
        }
        self.trim();
        rem as u32
    }
    fn add(&mut self, o: &Big) {
        let mut carry = 0u64;
        for i in 0..self.0.len().max(o.0.len()) {
            if i == self.0.len() {
                self.0.push(0);
            }
            let t = self.0[i] as u64 + *o.0.get(i).unwrap_or(&0) as u64 + carry;
            self.0[i] = t as u32;
            carry = t >> 32;
        }
        if carry > 0 {
            self.0.push(carry as u32);
        }
    }
    fn mul(&self, o: &Big) -> Big {
        let mut r = vec![0u32; self.0.len() + o.0.len()];
        for (i, &a) in self.0.iter().enumerate() {
            let mut carry = 0u64;
            for (j, &b) in o.0.iter().enumerate() {
                let t = r[i + j] as u64 + a as u64 * b as u64 + carry;
                r[i + j] = t as u32;
                carry = t >> 32;
            }
            let mut k = i + o.0.len();
            while carry > 0 {
                let t = r[k] as u64 + carry;
                r[k] = t as u32;
                carry = t >> 32;
                k += 1;
            }
        }
        let mut b = Big(r);
        b.trim();
        b
    }
    fn pow2(n: u64) -> Big {
        let mut v = vec![0u32; (n / 32) as usize + 1];
        v[(n / 32) as usize] = 1 << (n % 32);
        Big(v)
    }
    fn cmp(&self, o: &Big) -> Ordering {
        self.0
            .len()
            .cmp(&o.0.len())
            .then_with(|| self.0.iter().rev().cmp(o.0.iter().rev()))
    }
}

/// Exact decimal value of a finite non-negative f64 as printed (shortest
/// round-trip form): (mantissa, digits after the point).
fn decimal_of(x: f64) -> Result<(Big, u32)> {
    ensure!(
        x.is_finite() && x >= 0.0,
        "{x} is not a finite non-negative number"
    );
    let s = format!("{x}");
    let (int, frac) = s.split_once('.').unwrap_or((&s, ""));
    let mut m = Big(vec![]);
    for c in int.chars().chain(frac.chars()) {
        let d = c
            .to_digit(10)
            .with_context(|| format!("unexpected digit in {s}"))?;
        m.mul_small(10);
        m.add(&Big::from_u64(d as u64));
    }
    Ok((m, frac.len() as u32))
}

/// Smallest q ≤ limit with q ≥ factor · x² (x as its printed decimal), or None.
fn ceil_scaled_square(x: f64, factor: u64, limit: u64) -> Result<Option<u64>> {
    let (m, k) = decimal_of(x)?;
    let mut num = m.mul(&m);
    num = num.mul(&Big::from_u64(factor));
    let mut den = Big::from_u64(1);
    for _ in 0..2 * k {
        den.mul_small(10);
    }
    for q in 0..=limit {
        let mut lhs = den.clone();
        if q == 0 {
            lhs = Big(vec![]);
        } else {
            lhs.mul_small(q as u32);
        }
        if lhs.cmp(&num) != Ordering::Less {
            return Ok(Some(q));
        }
    }
    Ok(None)
}

/// Whether x (its printed decimal) > num/den, exactly.
fn decimal_gt(x: f64, num: u64, den: u64) -> Result<bool> {
    let (m, k) = decimal_of(x)?;
    let lhs = m.mul(&Big::from_u64(den));
    let mut rhs = Big::from_u64(num);
    for _ in 0..k {
        rhs.mul_small(10);
    }
    Ok(lhs.cmp(&rhs) == Ordering::Greater)
}

/// §12.3 (`r2_mock_batches_v2`): the T8a branch and B_M from s, computed
/// exactly on s's printed decimal ((2.80/σ_m)² = 6,400, (2.80/δ_m)² = 1,600).
/// Returns (branch, B_M, shortfall): the w/2 branch iff ⌈6400 s²⌉ ≤ 58;
/// otherwise δ with B_M = min(⌈1600 s²⌉, 58); B_M ≥ 1. The shortfall flag
/// is §12.3's "If s > 0.19, B_M = 58, and the shortfall is recorded", on
/// s's decimal value (s ≤ 0.19 gives ⌈1600 s²⌉ ≤ 58, and s > 0.19 gives
/// B_M = 58).
pub fn t8a_branch(s: f64) -> Result<(T8aBranch, u64, bool)> {
    let shortfall = decimal_gt(s, T8A_SHORTFALL_S.0, T8A_SHORTFALL_S.1)?;
    if let Some(b) = ceil_scaled_square(s, T8A_W_FACTOR, B_RES)? {
        return Ok((T8aBranch::WHalf, b.max(1), shortfall));
    }
    Ok(match ceil_scaled_square(s, T8A_DELTA_FACTOR, B_RES)? {
        Some(b) => (T8aBranch::Delta, b.max(1), shortfall),
        None => (T8aBranch::Delta, B_RES, shortfall),
    })
}

/// `r2_human_rare_v1`: whether P(Bin(b + c, ½) ≥ b) < 1/20, decided exactly
/// (20 · Σ_{i ≥ b} C(n, i) < 2^n), with the tail probability as f64 for the record.
pub fn exact_paired_fails(b: u64, c: u64) -> (bool, f64) {
    let n = b + c;
    let mut coef = Big::from_u64(1);
    let mut tail = Big(vec![]);
    let (mut lnc, mut p) = (0.0f64, 0.0f64);
    for i in 0..=n {
        if i >= b {
            tail.add(&coef);
            p += (lnc - n as f64 * std::f64::consts::LN_2).exp();
        }
        if i < n {
            coef.mul_small((n - i) as u32);
            let r = coef.div_small((i + 1) as u32);
            debug_assert_eq!(r, 0);
            lnc += ((n - i) as f64).ln() - ((i + 1) as f64).ln();
        }
    }
    tail.mul_small(EXACT_ALPHA_DEN);
    (tail.cmp(&Big::pow2(n)) == Ordering::Less, p.min(1.0))
}

// ------------------------------------------------------------------ normal quantile and r2_slice_gate_v2

/// Standard normal CDF by Marsaglia's (2004) series; absolute error near f64
/// rounding for |x| ≤ 9 (beyond, 0 or 1).
pub fn normal_cdf(x: f64) -> f64 {
    if x < -9.0 {
        return 0.0;
    }
    if x > 9.0 {
        return 1.0;
    }
    let q = x * x;
    let (mut s, mut t, mut b, mut i) = (x, 0.0f64, x, 1.0f64);
    while s != t {
        t = s;
        i += 2.0;
        b *= q / i;
        s = t + b;
    }
    0.5 + s * (-0.5 * q - 0.918_938_533_204_672_8).exp()
}

/// Standard normal quantile: Acklam's rational start, refined by Halley steps on [`normal_cdf`].
pub fn normal_quantile(p: f64) -> f64 {
    assert!(p > 0.0 && p < 1.0, "quantile of {p}");
    const A: [f64; 6] = [
        -3.969683028665376e1,
        2.209460984245205e2,
        -2.759285104469687e2,
        1.383_577_518_672_69e2,
        -3.066479806614716e1,
        2.506628277459239,
    ];
    const B: [f64; 5] = [
        -5.447609879822406e1,
        1.615858368580409e2,
        -1.556989798598866e2,
        6.680131188771972e1,
        -1.328068155288572e1,
    ];
    const C: [f64; 6] = [
        -7.784894002430293e-3,
        -3.223964580411365e-1,
        -2.400758277161838,
        -2.549732539343734,
        4.374664141464968,
        2.938163982698783,
    ];
    const D: [f64; 4] = [
        7.784695709041462e-3,
        3.224671290700398e-1,
        2.445134137142996,
        3.754408661907416,
    ];
    let tail = |q: f64| {
        (((((C[0] * q + C[1]) * q + C[2]) * q + C[3]) * q + C[4]) * q + C[5])
            / ((((D[0] * q + D[1]) * q + D[2]) * q + D[3]) * q + 1.0)
    };
    let mut x = if p < 0.02425 {
        tail((-2.0 * p.ln()).sqrt())
    } else if p > 1.0 - 0.02425 {
        -tail((-2.0 * (1.0 - p).ln()).sqrt())
    } else {
        let q = p - 0.5;
        let r = q * q;
        (((((A[0] * r + A[1]) * r + A[2]) * r + A[3]) * r + A[4]) * r + A[5]) * q
            / (((((B[0] * r + B[1]) * r + B[2]) * r + B[3]) * r + B[4]) * r + 1.0)
    };
    for _ in 0..4 {
        let e = normal_cdf(x) - p;
        let u = e * (2.0 * std::f64::consts::PI).sqrt() * (x * x / 2.0).exp();
        x -= u / (1.0 + x * u / 2.0);
    }
    x
}

/// z_K = Φ⁻¹(1 − 0.05/K) (`r2_slice_gate_v2`).
pub fn z_k(k: usize) -> f64 {
    assert!(k > 0);
    normal_quantile(1.0 - SLICE_ALPHA / k as f64)
}

/// `r2_slice_gate_v2`'s K and gated set from each candidate slice's planned SE
/// (§11.4, with amendment 15 item 15.3): a slice is gate-worthy at K when
/// z_K · SE_planned ≤ 0.030. K is the smallest K with at most K gate-worthy
/// slices at z_K, which is the fixed point of K ↦ |gate-worthy(K)| when one
/// exists (the count is non-increasing in K, so there is at most one);
/// without one, this K tests fewer slices than K at z_K, which is
/// conservative. Returns (K, z_K, gated flags); K = 0 when there is no
/// candidate slice.
pub fn slice_gate_k(planned_se: &[f64]) -> (usize, Option<f64>, Vec<bool>) {
    for k in 1..=planned_se.len() {
        let z = z_k(k);
        let worthy: Vec<bool> = planned_se.iter().map(|&s| z * s <= SLICE_BOUND).collect();
        if worthy.iter().filter(|x| **x).count() <= k {
            return (k, Some(z), worthy);
        }
    }
    (0, None, vec![false; planned_se.len()])
}

// ------------------------------------------------------------------ margins

fn runs<'a>(book: &'a Book, pop: &str, sys: &str) -> Result<&'a BTreeMap<u64, SeedRun>> {
    book.get(pop)
        .and_then(|m| m.get(sys))
        .with_context(|| format!("system {sys} has no predictions on population {pop}"))
}

/// The released seed's run, or the only one.
fn pick(seeds: &BTreeMap<u64, SeedRun>, released: u64) -> Result<&SeedRun> {
    seeds
        .get(&released)
        .or(if seeds.len() == 1 {
            seeds.values().next()
        } else {
            None
        })
        .with_context(|| format!("no released seed {released} and more than one seed"))
}

fn stat_value(pop: &Population, stat: Statistic, loss: &[f64]) -> f64 {
    match stat {
        Statistic::Brier => wmean(&pop.weights, loss),
        Statistic::HumanBrierBalanced => {
            let h: Vec<f64> = pop.humans.iter().map(|&p| loss[p]).collect();
            wmean(&pop.human_weights, &h)
        }
    }
}

fn stat_ess(pop: &Population, stat: Statistic) -> f64 {
    match stat {
        Statistic::Brier => kish_ess(&pop.weights),
        Statistic::HumanBrierBalanced => kish_ess(&pop.human_weights),
    }
}

/// Every margin, from comparator outputs only.
fn compute_margins(
    cfg: &Config,
    pops: &BTreeMap<String, Population>,
    book: &Book,
) -> Result<Value> {
    let comparators: BTreeSet<&str> = cfg.comparators.iter().map(String::as_str).collect();
    let candidates = gate_candidates(cfg);
    let mut out = Map::new();
    for m in &cfg.margins {
        ensure!(
            comparators.contains(m.system.as_str()),
            "margin {}: system {} is not a comparator",
            m.id,
            m.system
        );
        let pop = pops
            .get(&m.population)
            .with_context(|| format!("margin {}: unknown population {}", m.id, m.population))?;
        if m.statistic == Statistic::HumanBrierBalanced {
            ensure!(
                !pop.humans.is_empty(),
                "margin {}: population {} has no humans",
                m.id,
                m.population
            );
        }
        let seeds = runs(book, &m.population, &m.system)?;
        let per: BTreeMap<u64, f64> = seeds
            .iter()
            .map(|(s, r)| (*s, stat_value(pop, m.statistic, &r.loss)))
            .collect();
        let (value, artifact, fault) = match m.kind {
            MarginKind::ReseedSd => {
                let v: Vec<f64> = per.values().copied().collect();
                let sd = sample_sd(&v);
                let seeds_ok = per.keys().copied().eq(RECIPE_SEEDS);
                let fault = if !seeds_ok {
                    Some(format!(
                        "computed from seeds {:?}, not the comparator's three seeds {RECIPE_SEEDS:?} (item 1.10 (a))",
                        per.keys().collect::<Vec<_>>()
                    ))
                } else {
                    match sd {
                        Some(x) if x.is_finite() && x > 0.0 => None,
                        _ => Some("the margin is 0 or not finite (item 1.10 (a))".to_string()),
                    }
                };
                (sd, sd.map(|x| std::f64::consts::SQRT_2 * x), fault)
            }
            MarginKind::SigmaMB0 => {
                let r = pick(seeds, cfg.released_seed)?;
                let b0 = stat_value(pop, m.statistic, &r.loss);
                let v = SIGMA_M * b0;
                let fault = (!(v.is_finite() && v > 0.0))
                    .then(|| "the margin is 0 or not finite (item 1.10 (a))".to_string());
                (Some(v), Some(v), fault)
            }
        };
        out.insert(
            m.id.clone(),
            json!({
                "kind": match m.kind { MarginKind::ReseedSd => "reseed_sd", MarginKind::SigmaMB0 => "sigma_m_b0" },
                "statistic": m.statistic.name(),
                "population": m.population,
                "system": m.system,
                "per_seed": per.iter().map(|(s, v)| (s.to_string(), json!(v))).collect::<Map<_, _>>(),
                "sigma_m": (m.kind == MarginKind::SigmaMB0).then_some(SIGMA_M),
                "value": value,
                "artifact_value": artifact,
                "artifact_scale": if m.kind == MarginKind::ReseedSd { "sqrt(2) (r2_margin_scale_v1)" } else { "1 (not a reseed margin)" },
                "kish_ess": stat_ess(pop, m.statistic),
                "fault": fault,
            }),
        );
    }
    let mut inputs = vec![];
    for name in &cfg.comparators {
        for (pid, systems) in book {
            if let Some(seeds) = systems.get(name) {
                for r in seeds.values() {
                    inputs.push(json!({"system": name, "seed": r.seed, "population": pid, "rows_jsonl_sha256": pops[pid].rows_sha256, "inputs": r.inputs}));
                }
            }
        }
    }
    Ok(json!({
        "schema": MARGINS_SCHEMA,
        "protocol": PROTOCOL,
        "rule": MARGIN_RULE,
        "comparators": cfg.comparators,
        "gate_candidates": candidates,
        "null_check": cfg.null_check,
        "candidates_among_comparators": candidates.iter().filter(|c| comparators.contains(*c)).collect::<Vec<_>>(),
        "comparator_inputs": inputs,
        "margins": out,
    }))
}

/// Every system a gate evaluates.
fn gate_candidates(cfg: &Config) -> BTreeSet<&str> {
    cfg.gates.iter().flat_map(|g| g.candidates()).collect()
}

// ------------------------------------------------------------------ gates

struct Ctx<'a> {
    cfg: &'a Config,
    pops: &'a BTreeMap<String, Population>,
    book: &'a Book,
    margins: &'a Value,
}

fn seed_list(seeds: &BTreeMap<u64, SeedRun>) -> Vec<u64> {
    seeds.keys().copied().collect()
}

/// Per-primary-position loss for a level: the released (or only) seed, or the seed mean.
fn level_loss(
    seeds: &BTreeMap<u64, SeedRun>,
    level: Level,
    released: u64,
    recipe_side: bool,
) -> Result<(Vec<f64>, Vec<u64>)> {
    match level {
        Level::Artifact => {
            let r = pick(seeds, released)?;
            Ok((r.loss.clone(), vec![r.seed]))
        }
        Level::Recipe => {
            let s = seed_list(seeds);
            if recipe_side {
                ensure!(
                    s == RECIPE_SEEDS,
                    "recipe level needs seeds {RECIPE_SEEDS:?}, got {s:?}"
                );
            } else {
                ensure!(
                    s == RECIPE_SEEDS || s.len() == 1,
                    "a recipe comparator needs seeds {RECIPE_SEEDS:?} or one seed, got {s:?}"
                );
            }
            let rs: Vec<&SeedRun> = seeds.values().collect();
            let n = rs[0].loss.len();
            let v = (0..n)
                .map(|p| rs.iter().map(|r| r.loss[p]).sum::<f64>() / rs.len() as f64)
                .collect();
            Ok((v, s))
        }
    }
}

fn margin_value(
    ctx: &Ctx,
    m: &MarginRef,
    level: Level,
) -> Result<std::result::Result<(f64, String), String>> {
    match (&m.margin, m.value) {
        (Some(id), None) => {
            let rec = ctx.margins["margins"]
                .get(id)
                .with_context(|| format!("unknown margin {id}"))?;
            if let Some(f) = rec["fault"].as_str() {
                return Ok(Err(format!("margin {id}: {f}")));
            }
            let k = if level == Level::Artifact {
                "artifact_value"
            } else {
                "value"
            };
            Ok(Ok((
                rec[k].as_f64().context("margin value")?,
                format!("margins file {id} ({k})"),
            )))
        }
        (None, Some(v)) => {
            ensure!(v.is_finite(), "fixed margin {v}");
            Ok(Ok((v, "fixed".to_string())))
        }
        _ => bail!("a margin names exactly one of margin and value"),
    }
}

fn paired_gate(ctx: &Ctx, spec: &GateSpec) -> Result<Value> {
    let GateSpec::Paired {
        id,
        statistic,
        terms,
        candidate,
        comparator,
        comparator_recipe,
        levels,
        margin,
        strict,
    } = spec
    else {
        unreachable!()
    };
    ensure!(
        !terms.is_empty() && !levels.is_empty(),
        "gate {id}: needs terms and levels"
    );
    let mut by_level = Map::new();
    let mut reasons = vec![];
    let mut all_pass = true;
    for &level in levels {
        let mut ts = vec![];
        let mut term_rec = vec![];
        for t in terms {
            ensure!(
                t.coef.is_finite() && t.coef != 0.0,
                "gate {id}: bad coefficient"
            );
            let pop = ctx
                .pops
                .get(&t.population)
                .with_context(|| format!("gate {id}: unknown population {}", t.population))?;
            let cand = t
                .candidate
                .as_ref()
                .or(candidate.as_ref())
                .with_context(|| format!("gate {id}: no candidate"))?;
            let comp = t
                .comparator
                .as_ref()
                .or(comparator.as_ref())
                .with_context(|| format!("gate {id}: no comparator"))?;
            let comp = if level == Level::Recipe {
                t.comparator_recipe
                    .as_ref()
                    .or(comparator_recipe.as_ref())
                    .unwrap_or(comp)
            } else {
                comp
            };
            let (la, sa) = level_loss(
                runs(ctx.book, &t.population, cand)?,
                level,
                ctx.cfg.released_seed,
                true,
            )
            .with_context(|| format!("gate {id}: candidate {cand}"))?;
            let (lb, sb) = level_loss(
                runs(ctx.book, &t.population, comp)?,
                level,
                ctx.cfg.released_seed,
                false,
            )
            .with_context(|| format!("gate {id}: comparator {comp}"))?;
            let (pos, w): (Vec<usize>, &[f64]) = match statistic {
                Statistic::Brier => ((0..pop.primary.len()).collect(), &pop.weights),
                Statistic::HumanBrierBalanced => (pop.humans.clone(), &pop.human_weights),
            };
            ensure!(
                !pos.is_empty(),
                "gate {id}: population {} has no rows for {}",
                t.population,
                statistic.name()
            );
            term_rec.push(json!({"population": t.population, "block": pop.block.name(), "coef": t.coef, "candidate": cand, "comparator": comp,
                "seeds_candidate": sa, "seeds_comparator": sb, "documents": pos.len()}));
            ts.push(Term {
                block: pop.block,
                units: pos.iter().map(|&p| pop.units[p].as_str()).collect(),
                weights: w.to_vec(),
                delta: pos.iter().map(|&p| la[p] - lb[p]).collect(),
                coef: t.coef,
            });
        }
        let (point, blocks) = composite_units(&ts);
        let refs: Vec<&[f64]> = blocks.iter().map(|(_, v)| v.as_slice()).collect();
        let draws = stratified_bootstrap(&refs);
        let units: Map<String, Value> = blocks
            .iter()
            .map(|(b, v)| (b.name().to_string(), json!(v.len())))
            .collect();
        let (lower, upper) = match &draws {
            Some(d) => (Some(d[24]), Some(d[974])),
            None => (None, None),
        };
        let mut rec = json!({"terms": term_rec, "units": units, "point": point, "lower": lower, "upper": upper,
            "interval_includes_zero": lower.zip(upper).map(|(l, u)| l <= 0.0 && 0.0 <= u)});
        match margin {
            None => {}
            Some(m) => match margin_value(ctx, m, level)? {
                Err(why) => {
                    reasons.push(why);
                    all_pass = false;
                }
                Ok((mv, src)) => {
                    rec["margin"] = json!(mv);
                    rec["margin_source"] = json!(src);
                    rec["comparison"] = json!(if *strict {
                        "upper < margin"
                    } else {
                        "upper <= margin"
                    });
                    match upper {
                        None => {
                            reasons
                                .push(format!("{}: fewer than two units in a block", level.name()));
                            all_pass = false;
                        }
                        Some(u) => {
                            let pass = margin_pass(u, mv, *strict);
                            rec["pass"] = json!(pass);
                            all_pass &= pass;
                        }
                    }
                }
            },
        }
        by_level.insert(level.name().into(), rec);
    }
    let status = if margin.is_none() {
        "reported"
    } else if !reasons.is_empty() {
        "not_evaluated"
    } else if all_pass {
        "pass"
    } else {
        "fail"
    };
    Ok(
        json!({"id": id, "kind": "paired", "statistic": statistic.name(), "difference": "candidate_minus_comparator",
        "bootstrap": BOOTSTRAP_METHOD, "levels": by_level, "status": status, "reasons": reasons}),
    )
}

fn level_seeds(
    seeds: &BTreeMap<u64, SeedRun>,
    level: Level,
    released: u64,
) -> Result<Vec<&SeedRun>> {
    Ok(match level {
        Level::Artifact => vec![pick(seeds, released)?],
        Level::Recipe => {
            let s = seed_list(seeds);
            ensure!(
                s == RECIPE_SEEDS,
                "recipe level needs seeds {RECIPE_SEEDS:?}, got {s:?}"
            );
            seeds.values().collect()
        }
    })
}

fn threshold(r: &SeedRun, point: &str) -> Result<f64> {
    r.ops
        .as_ref()
        .and_then(|m| m.get(point))
        .copied()
        .with_context(|| {
            format!(
                "seed {} has no CAL-declared operating point {point}",
                r.seed
            )
        })
}

fn cp_gate(ctx: &Ctx, spec: &GateSpec) -> Result<Value> {
    let GateSpec::CpFlags {
        id,
        population,
        system,
        point,
        levels,
    } = spec
    else {
        unreachable!()
    };
    let pop = ctx
        .pops
        .get(population)
        .with_context(|| format!("gate {id}: unknown population {population}"))?;
    let n = pop.humans.len();
    ensure!(n > 0, "gate {id}: population {population} has no humans");
    let limit = max_flags(n as u64, CP_LEVEL, CP_BOUND);
    let seeds = runs(ctx.book, population, system)?;
    let mut by_level = Map::new();
    let mut all = true;
    for &level in levels {
        let mut per = vec![];
        let mut pass = true;
        let mut maxk = 0;
        for r in level_seeds(seeds, level, ctx.cfg.released_seed)
            .with_context(|| format!("gate {id}"))?
        {
            let t = threshold(r, point).with_context(|| format!("gate {id}: {system}"))?;
            let k = pop.humans.iter().filter(|&&p| r.flag[p] >= t).count();
            let cp = clopper_pearson_upper(k as u64, n as u64, CP_LEVEL);
            let ok = cp <= CP_BOUND;
            pass &= ok;
            maxk = maxk.max(k);
            per.push(json!({"seed": r.seed, "threshold_bits": format!("0x{:016x}", t.to_bits()), "flags": k, "humans": n,
                "clopper_pearson_upper_one_sided_95": cp, "pass": ok}));
        }
        all &= pass;
        by_level.insert(
            level.name().into(),
            json!({"seeds": per, "max_flags": maxk, "pass": pass}),
        );
    }
    Ok(
        json!({"id": id, "kind": "cp_flags", "population": population, "system": system, "point": point,
        "rule": "one-sided 95% Clopper-Pearson upper bound <= 2.5% at every seed (Tier C rule)", "flag_limit": limit,
        "levels": by_level, "status": if all { "pass" } else { "fail" }, "reasons": []}),
    )
}

fn exact_gate(ctx: &Ctx, spec: &GateSpec) -> Result<Value> {
    let GateSpec::ExactPaired {
        id,
        population,
        candidate,
        comparator,
        point,
        levels,
    } = spec
    else {
        unreachable!()
    };
    let pop = ctx
        .pops
        .get(population)
        .with_context(|| format!("gate {id}: unknown population {population}"))?;
    ensure!(
        !pop.humans.is_empty(),
        "gate {id}: population {population} has no humans"
    );
    let (sa, sb) = (
        runs(ctx.book, population, candidate)?,
        runs(ctx.book, population, comparator)?,
    );
    let mut by_level = Map::new();
    let mut all = true;
    for &level in levels {
        let pairs: Vec<(&SeedRun, &SeedRun)> = match level {
            Level::Artifact => vec![(
                pick(sa, ctx.cfg.released_seed)?,
                pick(sb, ctx.cfg.released_seed)?,
            )],
            Level::Recipe => {
                ensure!(
                    seed_list(sa) == RECIPE_SEEDS && seed_list(sb) == RECIPE_SEEDS,
                    "gate {id}: the recipe level pairs seeds {RECIPE_SEEDS:?} on both sides"
                );
                RECIPE_SEEDS.iter().map(|s| (&sa[s], &sb[s])).collect()
            }
        };
        let mut per = vec![];
        let mut pass = true;
        for (a, b) in pairs {
            let (ta, tb) = (threshold(a, point)?, threshold(b, point)?);
            let (mut nb, mut nc) = (0u64, 0u64);
            for &p in &pop.humans {
                match (a.flag[p] >= ta, b.flag[p] >= tb) {
                    (true, false) => nb += 1,
                    (false, true) => nc += 1,
                    _ => {}
                }
            }
            let (fails, pv) = exact_paired_fails(nb, nc);
            pass &= !fails;
            per.push(json!({"seed_candidate": a.seed, "seed_comparator": b.seed, "b_candidate_only": nb, "c_comparator_only": nc,
                "p_upper_tail": pv, "fails": fails}));
        }
        all &= pass;
        by_level.insert(level.name().into(), json!({"pairs": per, "pass": pass}));
    }
    Ok(
        json!({"id": id, "kind": "exact_paired", "rule": EXACT_RULE, "population": population, "candidate": candidate,
        "comparator": comparator, "point": point,
        "test": "exact one-sided conditional binomial (McNemar) on discordant human flags, each system at its own declared threshold; fail iff P(Bin(b + c, 1/2) >= b) < 0.05, decided in exact integers",
        "levels": by_level, "status": if all { "pass" } else { "fail" }, "reasons": []}),
    )
}

/// A noninferiority gate passes iff its 95% upper bound ≤ the margin; a
/// superiority gate (`strict`, e.g. T1b's "upper < 0") iff upper < margin.
pub fn margin_pass(upper: f64, margin: f64, strict: bool) -> bool {
    if strict {
        upper < margin
    } else {
        upper <= margin
    }
}

/// `r2_t8a_v2`'s criterion: w/2 branch `draws[974] ≤ σ_m`; δ branch
/// `draws[974] ≤ δ_m` and the point estimate ≤ 0.
pub fn t8a_pass(branch: T8aBranch, point: f64, upper: f64) -> bool {
    match branch {
        T8aBranch::WHalf => upper <= SIGMA_M,
        T8aBranch::Delta => upper <= DELTA_M && point <= 0.0,
    }
}

fn t8a_gate(ctx: &Ctx, spec: &GateSpec) -> Result<Value> {
    let GateSpec::T8a {
        id,
        population,
        candidate,
        comparator,
        comparator_recipe,
        levels,
        sealed,
        batch_field,
    } = spec
    else {
        unreachable!()
    };
    let pop = ctx
        .pops
        .get(population)
        .with_context(|| format!("gate {id}: unknown population {population}"))?;
    ensure!(
        pop.block == Block::Mock,
        "gate {id}: T8a runs on a mock population"
    );
    let (branch, b_m, s_shortfall) = t8a_branch(sealed.s)?;
    ensure!(
        branch == sealed.branch && b_m == sealed.b_m,
        "gate {id}: the sealed branch {} and B_M {} are not §12.3's {} and {b_m} for s = {}",
        sealed.branch.name(),
        sealed.b_m,
        branch.name(),
        sealed.s
    );
    // Amendment 8.1: the reserve check's record, digest and B_M′.
    let rc = &sealed.reserve_check;
    let digest = sha256_hex(&read(&rc.record)?);
    ensure!(
        digest == rc.test2_mock_reserve_check_sha256,
        "gate {id}: the reserve-check record {} hashes to {digest}, not the logged test2_mock_reserve_check_sha256 {}",
        rc.record.display(),
        rc.test2_mock_reserve_check_sha256
    );
    let b_m_prime = rc.b_m_prime;
    let n = pop.extra[batch_field]
        .iter()
        .collect::<BTreeSet<_>>()
        .len() as u64;
    let shortfall = json!({"rule": "PROTOCOL-A2 amendment 8.1: the mock is built at B_M' (the largest feasible B <= B_M); T8a's branch and decision rule stay as sealed; its result is reported with B_M - B_M' and the binding domains",
        "record": rc.record, "test2_mock_reserve_check_sha256": rc.test2_mock_reserve_check_sha256,
        "b_m": b_m, "b_m_prime": b_m_prime, "batches_short": b_m - b_m_prime,
        "binding_domains": rc.binding_domains, "realised_batches": n, "batch_field": batch_field});
    let head = |status: &str, levels: Value, reasons: Vec<String>| {
        json!({"id": id, "kind": "t8a", "rule": T8A_RULE, "population": population, "candidate": candidate,
        "statistic": "r = (B_cand - B_R0)/B_R0 pooled over all batches at the W_A8 batch weights; ratio bootstrap over ledger units",
        "sealed": {"s": sealed.s, "branch": sealed.branch.name(), "b_m": sealed.b_m, "s_above_0_19_shortfall": s_shortfall},
        "reserve_check": shortfall,
        "criterion": match branch { T8aBranch::WHalf => "draws[974] <= sigma_m = 0.035", T8aBranch::Delta => "draws[974] <= delta_m = 0.070 and point <= 0" },
        "levels": levels, "status": status, "reasons": reasons})
    };
    if b_m_prime == 0 {
        return Ok(head(
            "not_evaluated",
            json!({}),
            vec!["no B >= 1 is feasible (amendment 8.1): T8a is not evaluated; item 1.4 path (ii) applies".into()],
        ));
    }
    if n != b_m_prime {
        return Ok(head(
            "not_evaluated",
            json!({}),
            vec![format!(
                "the mock holds {n} batches in field {batch_field}, not the reserve check's B_M' {b_m_prime} (amendment 8.1)"
            )],
        ));
    }
    let mut by_level = Map::new();
    let mut all = true;
    let mut reasons = vec![];
    for &level in levels {
        let comp = if level == Level::Recipe {
            comparator_recipe.as_ref().unwrap_or(comparator)
        } else {
            comparator
        };
        let (lc, sa) = level_loss(
            runs(ctx.book, population, candidate)?,
            level,
            ctx.cfg.released_seed,
            true,
        )?;
        let (lr, sb) = level_loss(
            runs(ctx.book, population, comp)?,
            level,
            ctx.cfg.released_seed,
            false,
        )?;
        let mut u: BTreeMap<&str, (f64, f64)> = BTreeMap::new();
        for p in 0..pop.primary.len() {
            let e = u.entry(pop.units[p].as_str()).or_default();
            e.0 += pop.weights[p] * (lc[p] - lr[p]);
            e.1 += pop.weights[p] * lr[p];
        }
        let (d, r): (Vec<f64>, Vec<f64>) = u.into_values().unzip();
        let point = d.iter().sum::<f64>() / r.iter().sum::<f64>();
        let draws = bootstrap_draws(d.len(), |ix| {
            let (a, b) = ix
                .iter()
                .fold((0.0, 0.0), |acc, &j| (acc.0 + d[j], acc.1 + r[j]));
            a / b
        });
        let mut rec = json!({"seeds_candidate": sa, "seeds_comparator": sb, "comparator": comp, "ledger_units": d.len(), "point": point});
        match draws {
            None => {
                reasons.push(format!("{}: fewer than two ledger units", level.name()));
                all = false;
            }
            Some(dr) => {
                let upper = dr[974];
                let pass = t8a_pass(branch, point, upper);
                rec["lower"] = json!(dr[24]);
                rec["upper"] = json!(upper);
                rec["bootstrap_se"] = json!(sample_sd(&dr));
                rec["pass"] = json!(pass);
                all &= pass;
            }
        }
        by_level.insert(level.name().into(), rec);
    }
    let status = if !reasons.is_empty() {
        "not_evaluated"
    } else if all {
        "pass"
    } else {
        "fail"
    };
    Ok(head(status, Value::Object(by_level), reasons))
}

/// T1d: every primary document returns a completed result (a failed or
/// missing prediction fails), per population and seed of each level.
fn completion_gate(ctx: &Ctx, spec: &GateSpec) -> Result<Value> {
    let GateSpec::Completion {
        id,
        populations,
        system,
        levels,
    } = spec
    else {
        unreachable!()
    };
    ensure!(
        !populations.is_empty() && !levels.is_empty(),
        "gate {id}: needs populations and levels"
    );
    let mut by_level = Map::new();
    let mut all = true;
    for &level in levels {
        let mut per = vec![];
        let mut pass = true;
        for pid in populations {
            ensure!(
                ctx.pops.contains_key(pid),
                "gate {id}: unknown population {pid}"
            );
            let seeds = runs(ctx.book, pid, system)?;
            for r in level_seeds(seeds, level, ctx.cfg.released_seed)
                .with_context(|| format!("gate {id}"))?
            {
                let ok = r.failed == 0;
                pass &= ok;
                per.push(json!({"population": pid, "seed": r.seed, "documents": r.loss.len(),
                    "execution_failures": r.failed - r.missing, "missing_predictions": r.missing, "pass": ok}));
            }
        }
        all &= pass;
        by_level.insert(level.name().into(), json!({"seeds": per, "pass": pass}));
    }
    Ok(
        json!({"id": id, "kind": "completion", "system": system, "populations": populations,
        "rule": "T1d (PROTOCOL-A2 §11.2, v1.2 condition 4): every primary document returns a completed result; a failed or missing prediction fails",
        "levels": by_level, "status": if all { "pass" } else { "fail" }, "reasons": []}),
    )
}

fn not_evaluated(spec: &GateSpec, why: &str) -> Value {
    json!({"id": spec.id(), "status": "not_evaluated", "reasons": [why]})
}

fn blend_term<'a>(
    id: &str,
    pops: &'a BTreeMap<String, Population>,
    book: &'a Book,
    t: &BlendTerm,
    block: Block,
    form: Form,
) -> Result<(&'a Population, &'a BTreeMap<u64, SeedRun>)> {
    let p = pops
        .get(&t.population)
        .with_context(|| format!("blend {id}: unknown population {}", t.population))?;
    ensure!(
        p.block == block && p.form == Some(form),
        "blend {id}: population {} is not block {} form {form:?}",
        t.population,
        block.name()
    );
    Ok((p, runs(book, &t.population, &t.system)?))
}

/// `blended_brier_r2_v2` per seed and as the seed mean, for each blend spec.
pub fn blends(cfg: &Config, pops: &BTreeMap<String, Population>, book: &Book) -> Result<Value> {
    let mut out = Map::new();
    for b in &cfg.blends {
        let (ptn, stn) = blend_term(&b.id, pops, book, &b.t_n, Block::T, Form::N)?;
        let (ptr, str_) = blend_term(&b.id, pops, book, &b.t_r, Block::T, Form::R)?;
        let (pcn, scn) = blend_term(&b.id, pops, book, &b.c_n, Block::C, Form::N)?;
        let seeds = seed_list(stn);
        ensure!(
            seed_list(str_) == seeds && seed_list(scn) == seeds,
            "blend {}: the three systems' seeds differ",
            b.id
        );
        let mut per = Map::new();
        let mut vals = vec![];
        for s in &seeds {
            let tn = wmean(&ptn.weights, &stn[s].loss);
            let tr = wmean(&ptr.weights, &str_[s].loss);
            let cn = wmean(&pcn.weights, &scn[s].loss);
            let bt = 0.5 * tn + 0.5 * tr;
            let v = 0.5 * bt + 0.5 * cn;
            vals.push(v);
            per.insert(
                s.to_string(),
                json!({"B_T_N": tn, "B_T_R": tr, "B_T": bt, "B_C": cn, "blend": v}),
            );
        }
        out.insert(
            b.id.clone(),
            json!({"rule": BLEND_RULE, "t_n": b.t_n.system, "t_r": b.t_r.system, "c_n": b.c_n.system, "seeds": per,
                   "seed_mean": vals.iter().sum::<f64>() / vals.len() as f64}),
        );
    }
    Ok(Value::Object(out))
}

// ------------------------------------------------------------------ validation and run

fn validate(cfg: &Config) -> Result<()> {
    let mut pids = BTreeSet::new();
    for p in &cfg.populations {
        ensure!(
            pids.insert(p.id.as_str()),
            "population {} named twice",
            p.id
        );
    }
    let mut seen = BTreeSet::new();
    let mut names = BTreeSet::new();
    for s in &cfg.systems {
        ensure!(
            pids.contains(s.population.as_str()),
            "system {} names unknown population {}",
            s.name,
            s.population
        );
        ensure!(
            seen.insert((s.population.as_str(), s.name.as_str(), s.seed)),
            "system {} seed {} twice on {}",
            s.name,
            s.seed,
            s.population
        );
        names.insert(s.name.as_str());
    }
    for c in &cfg.comparators {
        ensure!(
            names.contains(c.as_str()),
            "comparator {c} has no predictions"
        );
    }
    let mut ids = BTreeSet::new();
    for m in &cfg.margins {
        ensure!(ids.insert(m.id.as_str()), "margin {} named twice", m.id);
    }
    let mut gids = BTreeSet::new();
    for g in &cfg.gates {
        ensure!(gids.insert(g.id()), "gate {} named twice", g.id());
        if let GateSpec::Paired {
            margin: Some(m),
            id,
            ..
        } = g
        {
            ensure!(
                m.margin.is_some() != m.value.is_some(),
                "gate {id}: a margin names exactly one of margin and value"
            );
            if let Some(r) = &m.margin {
                ensure!(ids.contains(r.as_str()), "gate {id}: unknown margin {r}");
            }
        }
        if let GateSpec::T8a { sealed, id, .. } = g {
            let (b, n, _) = t8a_branch(sealed.s)?;
            ensure!(
                b == sealed.branch && n == sealed.b_m,
                "gate {id}: the sealed T8a branch and B_M do not follow §12.3 from s"
            );
            let rc = &sealed.reserve_check;
            ensure!(
                rc.b_m == sealed.b_m,
                "gate {id}: the reserve check's B_M {} is not the sealed B_M {}",
                rc.b_m,
                sealed.b_m
            );
            ensure!(
                rc.b_m_prime <= rc.b_m,
                "gate {id}: the reserve check's B_M' {} exceeds B_M {}",
                rc.b_m_prime,
                rc.b_m
            );
            ensure!(
                (rc.b_m_prime < rc.b_m) == !rc.binding_domains.is_empty(),
                "gate {id}: a reserve check names binding domains exactly when B_M' < B_M (amendment 8.1)"
            );
            let h = &rc.test2_mock_reserve_check_sha256;
            ensure!(
                h.len() == 64
                    && h.bytes()
                        .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c)),
                "gate {id}: test2_mock_reserve_check_sha256 is not a SHA-256 hex digest"
            );
        }
    }
    if !cfg.null_check {
        let comparators: BTreeSet<&str> = cfg.comparators.iter().map(String::as_str).collect();
        for c in gate_candidates(cfg) {
            ensure!(
                !comparators.contains(c),
                "system {c} is a gate candidate and is listed as a comparator: margins come from comparator outputs only, read before any candidate output (§2.9); only a null-check configuration (§9.1 step 4) may list a candidate"
            );
        }
    }
    Ok(())
}

/// Extra row fields each population must load (batch fields of T8a gates).
fn extra_fields(cfg: &Config) -> BTreeMap<String, BTreeSet<String>> {
    let mut m: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for g in &cfg.gates {
        if let GateSpec::T8a {
            population,
            batch_field,
            ..
        } = g
        {
            m.entry(population.clone())
                .or_default()
                .insert(batch_field.clone());
        }
    }
    m
}

fn load_systems(
    cfg: &Config,
    pops: &BTreeMap<String, Population>,
    book: &mut Book,
    which: impl Fn(&str) -> bool,
) -> Result<()> {
    for s in cfg.systems.iter().filter(|s| which(&s.name)) {
        let run = load_run(&pops[&s.population], s)?;
        book.entry(s.population.clone())
            .or_default()
            .entry(s.name.clone())
            .or_default()
            .insert(s.seed, run);
    }
    Ok(())
}

/// The N/R pairs: N-form and R-form populations of the same block and split.
fn form_pairs(pops: &BTreeMap<String, Population>) -> Vec<(&Population, &Population)> {
    let mut v = vec![];
    for n in pops.values().filter(|p| p.form == Some(Form::N)) {
        for r in pops
            .values()
            .filter(|p| p.form == Some(Form::R) && p.block == n.block && p.split == n.split)
        {
            v.push((n, r));
        }
    }
    v
}

/// An R-form population holds its N-form population's humans unchanged.
fn check_form_humans(pops: &BTreeMap<String, Population>) -> Result<()> {
    for (n, r) in form_pairs(pops) {
        ensure!(
            n.human_ids() == r.human_ids(),
            "populations {} and {}: R-form humans must be the N-form humans unchanged",
            n.id,
            r.id
        );
    }
    Ok(())
}

/// A system scored on both populations of an N/R pair scores their humans identically.
fn check_forms(pops: &BTreeMap<String, Population>, book: &Book) -> Result<Value> {
    let mut pairs = vec![];
    for (n, r) in form_pairs(pops) {
        let pos_r: BTreeMap<&str, usize> = r
            .humans
            .iter()
            .map(|&p| (r.rows[r.primary[p]].id.as_str(), p))
            .collect();
        let mut systems = 0;
        for (name, sn) in book.get(&n.id).into_iter().flatten() {
            let Some(sr) = book.get(&r.id).and_then(|m| m.get(name)) else {
                continue;
            };
            systems += 1;
            for (seed, a) in sn {
                let Some(b) = sr.get(seed) else { continue };
                for &p in &n.humans {
                    let q = pos_r[n.rows[n.primary[p]].id.as_str()];
                    ensure!(
                        a.loss[p].to_bits() == b.loss[q].to_bits()
                            && a.flag[p].to_bits() == b.flag[q].to_bits(),
                        "{name} seed {seed}: human {} scores differently in {} and {}",
                        n.rows[n.primary[p]].id,
                        n.id,
                        r.id
                    );
                }
            }
        }
        pairs.push(json!({"n_form": n.id, "r_form": r.id, "humans": n.humans.len(), "systems_checked": systems}));
    }
    Ok(json!(pairs))
}

/// Evaluate a configuration: margins first (written to `margins_path`), then
/// the per-seed equality check, blends and gates; the evaluation is written
/// to `out_path` (a new file) and returned.
pub fn run(cfg: &Config, margins_path: &Path, out_path: &Path) -> Result<Value> {
    ensure!(cfg.schema == CONFIG_SCHEMA, "configuration schema");
    validate(cfg)?;
    let sealed = check_splits(cfg)?;
    ensure!(
        !out_path.exists(),
        "{} exists: a new output is required",
        out_path.display()
    );
    let extra = extra_fields(cfg);
    let mut pops = BTreeMap::new();
    for p in &cfg.populations {
        let e = extra.get(&p.id).cloned().unwrap_or_default();
        pops.insert(
            p.id.clone(),
            load_population(p, &e).with_context(|| format!("population {}", p.id))?,
        );
    }
    check_form_humans(&pops)?;
    // 1. Margins, from comparator outputs only, written before any difference.
    let comparators: BTreeSet<&str> = cfg.comparators.iter().map(String::as_str).collect();
    let mut book: Book = BTreeMap::new();
    load_systems(cfg, &pops, &mut book, |n| comparators.contains(n))?;
    let margins = compute_margins(cfg, &pops, &book)?;
    let mbytes = pretty(&margins)?;
    if margins_path.exists() {
        ensure!(
            read(margins_path)? == mbytes,
            "{} exists and differs from the margins recomputed from the comparator outputs",
            margins_path.display()
        );
    } else {
        write_new(margins_path, &mbytes)?;
    }
    let msha = sha256_hex(&mbytes);
    // 2. Every other system.
    load_systems(cfg, &pops, &mut book, |n| !comparators.contains(n))?;
    let forms = check_forms(&pops, &book)?;
    let mut values = Map::new();
    for (pid, systems) in &book {
        let mut sm = Map::new();
        for (name, seeds) in systems {
            let per: Map<String, Value> = seeds
                .values()
                .map(|r| (r.seed.to_string(), seed_values(&pops[pid], r)))
                .collect();
            sm.insert(name.clone(), Value::Object(per));
        }
        values.insert(pid.clone(), Value::Object(sm));
    }
    let values = Value::Object(values);
    let eq = equality(cfg, &pops, &book, &values)?;
    let equal = eq["pass"] == true;
    const UNEQUAL: &str =
        "per-seed values differ from the bound evaluator's, or could not be compared (S2 rule)";
    // Blends (and any selection built on them) need the per-seed equality, as gates do.
    let blend = if equal {
        blends(cfg, &pops, &book)?
    } else {
        Value::Object(
            cfg.blends
                .iter()
                .map(|b| {
                    (
                        b.id.clone(),
                        json!({"status": "not_evaluated", "reasons": [UNEQUAL]}),
                    )
                })
                .collect(),
        )
    };
    ensure!(
        sha256_hex(&read(margins_path)?) == msha,
        "the margins file changed during the run"
    );
    let ctx = Ctx {
        cfg,
        pops: &pops,
        book: &book,
        margins: &margins,
    };
    let mut gates = vec![];
    for g in &cfg.gates {
        gates.push(if !equal {
            not_evaluated(g, UNEQUAL)
        } else {
            match g {
                GateSpec::Paired { .. } => paired_gate(&ctx, g)?,
                GateSpec::CpFlags { .. } => cp_gate(&ctx, g)?,
                GateSpec::ExactPaired { .. } => exact_gate(&ctx, g)?,
                GateSpec::T8a { .. } => t8a_gate(&ctx, g)?,
                GateSpec::Completion { .. } => completion_gate(&ctx, g)?,
            }
        });
    }
    let out = json!({
        "schema": EVAL_SCHEMA,
        "protocol": PROTOCOL,
        "bound_evaluator": BOUND_EVALUATOR,
        "bootstrap": BOOTSTRAP_METHOD,
        "released_seed": cfg.released_seed,
        "consumed_sealed_splits": sealed,
        "margins": {"path": margins_path, "sha256": msha, "order": "written from comparator outputs before any other system was read",
                    "null_check": cfg.null_check, "gate_candidates": gate_candidates(cfg)},
        "populations": pops.iter().map(|(k, p)| (k.clone(), p.summary())).collect::<Map<_, _>>(),
        "n_r_pairs": forms,
        "systems": values,
        "per_seed_equality": eq,
        "blends": blend,
        "gates": gates,
        "pending_part_2": PENDING,
    });
    write_new(out_path, &pretty(&out)?)?;
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::comp_eval::group_values;
    use crate::pangram_subsample::bootstrap_interval;

    fn unif(s: &str) -> f64 {
        let d = Sha256::digest(s.as_bytes());
        (u64::from_le_bytes(d[..8].try_into().unwrap()) >> 11) as f64 / (1u64 << 53) as f64
    }

    /// The tracked `cc_launch_shares_v5` table (pinned by [`SHARES_SHA256`]).
    const SHARES: &[u8] = include_bytes!("../../grammar-cc-human/rules/launch-shares-v5.json");
    const T_SPLIT: &str = "deviter2_t";
    const C_SPLIT: &str = "deviter2_c";
    const MOCK_SPLIT: &str = "deviter2_mock";

    fn row(
        id: &str,
        split: &str,
        origin: &str,
        group: &str,
        domain: &str,
        population: &str,
    ) -> Value {
        let human = origin == "human_only";
        json!({"id": id, "split": split, "origin": origin,
               "share": if human { 0.0 } else if origin == "model_only" { 1.0 } else { 0.4 },
               "source_group": group, "domain": domain,
               "generator_label": if human { Value::Null } else if population == "frontier" { json!("claude-test") } else { json!("gen-a") },
               "operation": if human { Value::Null } else { json!("mirror") },
               "population": population, "text": "synthetic text, never read"})
    }

    fn t_rows(form: &str) -> Vec<Value> {
        let mut v = vec![];
        for g in 0..24 {
            let d = if g % 2 == 0 { "wikipedia" } else { "reddit" };
            let grp = format!("tg{g}");
            for k in 0..(g % 3) + 1 {
                v.push(row(
                    &format!("th-{g}-{k}"),
                    T_SPLIT,
                    "human_only",
                    &grp,
                    d,
                    "primary",
                ));
            }
            v.push(row(
                &format!("t{form}-{g}-0"),
                T_SPLIT,
                "mixed",
                &grp,
                d,
                "primary",
            ));
            if g % 2 == 0 {
                v.push(row(
                    &format!("t{form}-{g}-1"),
                    T_SPLIT,
                    "model_only",
                    &grp,
                    d,
                    "primary",
                ));
            }
            if g % 6 == 0 {
                v.push(row(
                    &format!("t{form}f-{g}"),
                    T_SPLIT,
                    "model_only",
                    &grp,
                    d,
                    "frontier",
                ));
            }
        }
        for r in &mut v {
            r["block"] = json!("T");
        }
        v
    }

    fn c_rows() -> Vec<Value> {
        let strata = ["news", "essays", "email"];
        let mut v = vec![];
        for g in 0..18 {
            let d = strata[g % 3];
            let grp = format!("cg{g}.example");
            for k in 0..(g % 2) + 1 {
                v.push(row(
                    &format!("ch-{g}-{k}"),
                    C_SPLIT,
                    "human_only",
                    &grp,
                    d,
                    "primary",
                ));
            }
            v.push(row(
                &format!("cm-{g}"),
                C_SPLIT,
                "mixed",
                &grp,
                d,
                "primary",
            ));
            if g % 4 != 3 {
                v.push(row(
                    &format!("co-{g}"),
                    C_SPLIT,
                    "model_only",
                    &grp,
                    d,
                    "primary",
                ));
            }
        }
        for r in &mut v {
            r["block"] = json!("C");
        }
        v
    }

    /// 12 ledger units over 5 source groups: every unit's rows span two or
    /// more groups and every group holds rows of several units, so units and
    /// groups give different bootstraps; W_A8 weights vary by row.
    fn mock_rows() -> Vec<Value> {
        let mut v = vec![];
        for u in 0..12usize {
            let mut add = |id: String, origin: &str, k: usize| {
                let grp = format!("mg{}", (u + k) % 5);
                let mut r = row(&id, MOCK_SPLIT, origin, &grp, "wikipedia", "primary");
                r["ledger_unit"] = json!(format!("lu{u}"));
                r["batch"] = json!(u % 2);
                r["w_a8"] = json!(1.0 + ((u + k) % 3) as f64 * 0.5 + (u % 4) as f64 * 0.25);
                v.push(r);
            };
            add(format!("mh-{u}"), "human_only", 0);
            add(format!("mx-{u}-0"), "mixed", 1);
            add(format!("mx-{u}-1"), "mixed", 2);
            if u % 3 != 0 {
                add(format!("mo-{u}"), "model_only", 3);
            }
        }
        v
    }

    fn pred_line(system: &str, seed: u64, r: &Value) -> Option<Value> {
        // Twin.p is Cand.p's exact copy (the zero-difference tie case).
        let system = if system == "Twin.p" { "Cand.p" } else { system };
        let id = r["id"].as_str().unwrap();
        if system == "Cand.p" && seed == 41 && id == "th-3-0" {
            return None; // missing: scored worst case
        }
        if system == "Cand.p" && seed == 29 && id == "tN-1-0" {
            return Some(json!({"id": id, "status": "failed", "failure": "timeout"}));
        }
        let u = unif(&format!("{system}:{seed}:{id}"));
        let inv = if r["origin"] == "human_only" {
            0.05 + 0.5 * u
        } else {
            0.35 + 0.6 * u
        };
        let pm = inv * 0.6;
        Some(
            json!({"id": id, "p_human": 1.0 - inv, "p_model_only": pm, "p_mixed": inv - pm, "share": pm}),
        )
    }

    struct Fixture {
        dir: tempfile::TempDir,
        cfg: Value,
    }

    fn write_lines(p: &Path, v: &[Value]) {
        let s: Vec<String> = v.iter().map(|x| x.to_string()).collect();
        std::fs::write(p, s.join("\n") + "\n").unwrap();
    }

    fn reserve_check(d: &Path, tag: &str, b_m: u64, b_m_prime: u64, binding: &[&str]) -> Value {
        let p = d.join(format!("reserve-check-{tag}.json"));
        let body = json!({"synthetic": true, "b_m": b_m, "b_m_prime": b_m_prime, "binding_domains": binding});
        std::fs::write(&p, body.to_string()).unwrap();
        json!({"record": p, "test2_mock_reserve_check_sha256": sha256_hex(&std::fs::read(&p).unwrap()),
               "b_m": b_m, "b_m_prime": b_m_prime, "binding_domains": binding})
    }

    fn fixture() -> Fixture {
        let dir = tempfile::tempdir().unwrap();
        let d = dir.path();
        let pops: [(&str, &str, Vec<Value>); 4] = [
            ("T.N", "tn", t_rows("N")),
            ("T.R", "tr", t_rows("R")),
            ("C.N", "cn", c_rows()),
            ("mock", "mock", mock_rows()),
        ];
        std::fs::write(d.join("shares.json"), SHARES).unwrap();
        let mut systems = vec![];
        let plan: [(&str, &str, &[u64]); 15] = [
            ("T.N", "Cand.p", &RECIPE_SEEDS),
            ("T.N", "R0.p", &[17]),
            ("T.N", "Recal.p", &RECIPE_SEEDS),
            ("T.N", "Faux.u", &RECIPE_SEEDS),
            ("T.N", "Cand.u", &RECIPE_SEEDS),
            ("T.N", "R0.u", &[17]),
            ("T.N", "Twin.p", &RECIPE_SEEDS),
            ("T.R", "Cand.p", &RECIPE_SEEDS),
            ("T.R", "R0.p", &[17]),
            ("T.R", "Recal.p", &RECIPE_SEEDS),
            ("C.N", "Cand.m", &RECIPE_SEEDS),
            ("C.N", "Recal.m", &RECIPE_SEEDS),
            ("mock", "Cand.t", &RECIPE_SEEDS),
            ("mock", "R0.t", &[17]),
            ("mock", "R0.t3", &RECIPE_SEEDS),
        ];
        let mut cfg_pops = vec![];
        for (id, file, rows) in &pops {
            let p = d.join(format!("{file}.jsonl"));
            write_lines(&p, rows);
            let split = match *id {
                "T.N" | "T.R" => T_SPLIT,
                "C.N" => C_SPLIT,
                _ => MOCK_SPLIT,
            };
            let mut spec = json!({"id": id, "split": split, "rows": p});
            match *id {
                "T.N" | "T.R" => {
                    spec["block"] = json!("T");
                    spec["form"] = json!(if *id == "T.N" { "N" } else { "R" });
                    spec["weights"] = json!({"rule": "balanced"});
                }
                "C.N" => {
                    spec["block"] = json!("C");
                    spec["form"] = json!("N");
                    spec["weights"] = json!({"rule": "cohort", "shares": d.join("shares.json")});
                }
                _ => {
                    spec["block"] = json!("mock");
                    spec["weights"] = json!({"rule": "field", "field": "w_a8"});
                    spec["unit_field"] = json!("ledger_unit");
                }
            }
            cfg_pops.push(spec);
            for (pop, name, seeds) in plan.iter().filter(|x| x.0 == *id) {
                for &seed in seeds.iter() {
                    let pp = d.join(format!("pred-{file}-{name}-{seed}.jsonl"));
                    let lines: Vec<Value> =
                        rows.iter().filter_map(|r| pred_line(name, seed, r)).collect();
                    write_lines(&pp, &lines);
                    let mut s =
                        json!({"name": name, "seed": seed, "population": pop, "predictions": pp});
                    let thr = match *name {
                        "Cand.p" | "Twin.p" => Some((0.50, 0.53)),
                        "Recal.p" => Some((0.51, 0.52)),
                        _ => None,
                    };
                    if let Some((a, b)) = thr {
                        let op = d.join(format!("ops-{name}-{seed}.json"));
                        std::fs::write(&op, json!({"0.01": a, "0.001": b}).to_string()).unwrap();
                        s["ops"] = json!(op);
                    }
                    systems.push(s);
                }
            }
        }
        let rc = reserve_check(d, "fixture", 2, 2, &[]);
        let cfg = json!({
            "schema": CONFIG_SCHEMA,
            "populations": cfg_pops,
            "systems": systems,
            "comparators": ["R0.p", "Recal.p", "Faux.u", "R0.u", "Recal.m", "R0.t"],
            "margins": [
                {"id": "m_H.T", "kind": "reseed_sd", "statistic": "human_brier_balanced", "population": "T.N", "system": "Faux.u"},
                {"id": "wT2.T1a", "kind": "sigma_m_b0", "statistic": "brier", "population": "T.N", "system": "R0.p"},
                {"id": "wT2.T1e", "kind": "sigma_m_b0", "statistic": "brier", "population": "T.R", "system": "R0.p"}
            ],
            "blends": [
                {"id": "Cand", "t_n": {"population": "T.N", "system": "Cand.p"}, "t_r": {"population": "T.R", "system": "Cand.p"}, "c_n": {"population": "C.N", "system": "Cand.m"}},
                {"id": "Recal", "t_n": {"population": "T.N", "system": "Recal.p"}, "t_r": {"population": "T.R", "system": "Recal.p"}, "c_n": {"population": "C.N", "system": "Recal.m"}}
            ],
            "gates": [
                {"kind": "paired", "id": "T1a", "statistic": "brier", "terms": [{"population": "T.N"}], "candidate": "Cand.p", "comparator": "R0.p",
                 "levels": ["artifact", "recipe"], "margin": {"margin": "wT2.T1a"}},
                {"kind": "paired", "id": "T1e", "statistic": "brier", "terms": [{"population": "T.R"}], "candidate": "Cand.p", "comparator": "R0.p",
                 "levels": ["artifact", "recipe"], "margin": {"margin": "wT2.T1e"}},
                {"kind": "paired", "id": "T1b", "statistic": "brier", "terms": [{"population": "C.N"}], "candidate": "Cand.m", "comparator": "Recal.m",
                 "levels": ["artifact"], "margin": {"value": 0.0}, "strict": true},
                {"kind": "paired", "id": "T1c.T", "statistic": "brier", "terms": [{"population": "T.N"}], "candidate": "Cand.p", "comparator": "Recal.p",
                 "levels": ["artifact", "recipe"], "margin": {"value": V12_DELTA}},
                {"kind": "completion", "id": "T1d", "populations": ["T.N", "T.R"], "system": "Cand.p", "levels": ["artifact", "recipe"]},
                {"kind": "paired", "id": "T3c.T", "statistic": "human_brier_balanced", "terms": [{"population": "T.N"}], "candidate": "Cand.u",
                 "comparator": "R0.u", "comparator_recipe": "Faux.u", "levels": ["artifact", "recipe"], "margin": {"margin": "m_H.T"}},
                {"kind": "cp_flags", "id": "T3a.T", "population": "T.N", "system": "Cand.p", "point": "0.01", "levels": ["artifact", "recipe"]},
                {"kind": "exact_paired", "id": "T3b", "population": "T.N", "candidate": "Cand.p", "comparator": "Recal.p", "point": "0.001",
                 "levels": ["artifact", "recipe"]},
                {"kind": "t8a", "id": "T8a", "population": "mock", "candidate": "Cand.t", "comparator": "R0.t", "levels": ["artifact", "recipe"],
                 "sealed": {"s": 0.015, "branch": "w_half", "b_m": 2, "reserve_check": rc}, "batch_field": "batch"},
                {"kind": "t8a", "id": "T8a.rc", "population": "mock", "candidate": "Cand.t", "comparator": "R0.t", "comparator_recipe": "R0.t3",
                 "levels": ["recipe"], "sealed": {"s": 0.015, "branch": "w_half", "b_m": 2, "reserve_check": rc}, "batch_field": "batch"},
                {"kind": "paired", "id": "tie", "statistic": "brier", "levels": ["recipe"], "terms": [
                    {"population": "T.N", "coef": 0.25, "candidate": "Cand.p", "comparator": "Recal.p"},
                    {"population": "T.R", "coef": 0.25, "candidate": "Cand.p", "comparator": "Recal.p"},
                    {"population": "C.N", "coef": 0.5, "candidate": "Cand.m", "comparator": "Recal.m"}]},
                {"kind": "paired", "id": "tie0", "statistic": "brier", "levels": ["artifact", "recipe"], "terms": [{"population": "T.N"}],
                 "candidate": "Cand.p", "comparator": "Twin.p"}
            ]
        });
        Fixture { dir, cfg }
    }

    fn set_gate(cfg: &mut Value, id: &str, f: impl FnOnce(&mut Value)) {
        f(cfg["gates"]
            .as_array_mut()
            .unwrap()
            .iter_mut()
            .find(|g| g["id"] == id)
            .unwrap());
    }

    /// The bound evaluator's output for one population, as a_comp_evaluate writes it (inputs block included).
    fn release_eval(fx: &Fixture, pop: &str, comparisons: &[(String, String)]) -> (PathBuf, Value) {
        let cfg = parse_config(&serde_json::to_vec(&fx.cfg).unwrap()).unwrap();
        let spec = cfg.populations.iter().find(|p| p.id == pop).unwrap();
        let rows: Vec<Row> = parse_rows(&std::fs::read_to_string(&spec.rows).unwrap()).unwrap();
        let mut systems: BTreeMap<String, SystemInput> = BTreeMap::new();
        let mut files = Map::new();
        for s in cfg.systems.iter().filter(|s| s.population == pop) {
            let b = std::fs::read(&s.predictions).unwrap();
            files.insert(
                format!("pred:{}:{}", s.name, s.seed),
                json!({"path": s.predictions, "sha256": sha256_hex(&b)}),
            );
            let ops = s.ops.as_ref().map(|o| {
                let ob = std::fs::read(o).unwrap();
                files.insert(
                    format!("ops:{}:{}", s.name, s.seed),
                    json!({"path": o, "sha256": sha256_hex(&ob)}),
                );
                parse_operating_points(&serde_json::from_slice(&ob).unwrap()).unwrap()
            });
            systems
                .entry(s.name.clone())
                .or_insert_with(|| SystemInput {
                    name: s.name.clone(),
                    seeds: vec![],
                    subsample: None,
                    missing: Missing::Worst,
                })
                .seeds
                .push(SeedInput {
                    seed: s.seed,
                    predictions: parse_predictions(std::str::from_utf8(&b).unwrap()).unwrap(),
                    operating_points: ops,
                });
        }
        let inputs: Vec<SystemInput> = systems.into_values().collect();
        let mut e = comp_eval::evaluate(&rows, &inputs, comparisons, &Options::default()).unwrap();
        e["inputs"] = json!({"rows_jsonl_sha256": sha256_hex(&std::fs::read(&spec.rows).unwrap()), "files": files});
        let p = fx
            .dir
            .path()
            .join(format!("release-{pop}-{}.json", comparisons.len()));
        std::fs::write(&p, serde_json::to_vec(&e).unwrap()).unwrap();
        (p, e)
    }

    fn run_value(fx: &Fixture, cfg: &Value, tag: &str) -> Result<Value> {
        let c = parse_config(&serde_json::to_vec(cfg).unwrap())?;
        run(
            &c,
            &fx.dir.path().join(format!("{tag}-margins.json")),
            &fx.dir.path().join(format!("{tag}-out.json")),
        )
    }

    fn gate<'a>(out: &'a Value, id: &str) -> &'a Value {
        out["gates"]
            .as_array()
            .unwrap()
            .iter()
            .find(|g| g["id"] == id)
            .unwrap()
    }

    fn f(v: &Value) -> f64 {
        v.as_f64().unwrap_or_else(|| panic!("not a number: {v}"))
    }

    fn err(r: Result<Value>) -> String {
        format!("{:#}", r.expect_err("expected an error"))
    }

    #[test]
    fn r2_eval_end_to_end_with_release_evaluation() {
        let fx = fixture();
        let (p, _) = release_eval(&fx, "T.N", &[]);
        let mut cfg = fx.cfg.clone();
        cfg["release_evaluations"] = json!({"T.N": p});
        let out = run_value(&fx, &cfg, "e2e").unwrap();
        let eq = &out["per_seed_equality"];
        assert_eq!(eq["pass"], true, "{eq}");
        // 4 populations; the release evaluation repeats T.N's checks.
        assert!(eq["values_checked"].as_u64().unwrap() > 200);
        for g in out["gates"].as_array().unwrap() {
            assert_ne!(g["status"], "not_evaluated", "{g}");
        }
        assert_eq!(gate(&out, "tie")["status"], "reported");
        assert!(gate(&out, "tie")["levels"]["recipe"]["interval_includes_zero"].is_boolean());
        assert_eq!(
            out["systems"]["T.N"]["Cand.p"]["29"]["bound"]["execution_failures"],
            1
        );
        assert_eq!(
            out["systems"]["T.N"]["Cand.p"]["41"]["missing_predictions"],
            1
        );
        // Files: margins and output exist, mode 600, and the recorded digest is the margins file's.
        let m = std::fs::read(fx.dir.path().join("e2e-margins.json")).unwrap();
        assert_eq!(out["margins"]["sha256"], sha256_hex(&m).as_str());
        let on_disk: Value =
            serde_json::from_slice(&std::fs::read(fx.dir.path().join("e2e-out.json")).unwrap())
                .unwrap();
        assert_eq!(on_disk, out);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            for f in ["e2e-out.json", "e2e-margins.json"] {
                let mode = std::fs::metadata(fx.dir.path().join(f))
                    .unwrap()
                    .permissions()
                    .mode();
                assert_eq!(mode & 0o777, 0o600, "{f}");
            }
        }
        assert_eq!(
            out["pending_part_2"].as_array().unwrap().len(),
            PENDING.len()
        );
        // A second run into the same output is refused.
        assert!(run_value(&fx, &cfg, "e2e").is_err());
    }

    #[test]
    fn r2_eval_t_block_paired_equals_bound_comparison() {
        let fx = fixture();
        let out = run_value(&fx, &fx.cfg, "cmp").unwrap();
        let (_, e) = release_eval(
            &fx,
            "T.N",
            &[
                ("Cand.p".into(), "R0.p".into()),
                ("Cand.p".into(), "Recal.p".into()),
            ],
        );
        for (gid, ci) in [("T1a", 0usize), ("T1c.T", 1)] {
            let g = &gate(&out, gid)["levels"];
            for (lvl, variant) in [("artifact", "released"), ("recipe", "recipe")] {
                let b = &e["comparisons"][ci]["variants"][variant];
                assert_eq!(
                    f(&g[lvl]["point"]).to_bits(),
                    f(&b["balanced_brier_difference"]).to_bits(),
                    "{gid} {lvl}"
                );
                assert_eq!(
                    f(&g[lvl]["lower"]).to_bits(),
                    f(&b["balanced_brier_difference_95ci"]["lower"]).to_bits(),
                    "{gid} {lvl}"
                );
                assert_eq!(
                    f(&g[lvl]["upper"]).to_bits(),
                    f(&b["balanced_brier_difference_95ci"]["upper"]).to_bits(),
                    "{gid} {lvl}"
                );
            }
        }
    }

    #[test]
    fn r2_eval_human_only_paired_equals_s2_h1() {
        // T3c's artifact level is the S2 H1 interval of a_spans_human_gates: balanced
        // weights over the primary humans, group values, the protocol bootstrap.
        let fx = fixture();
        let out = run_value(&fx, &fx.cfg, "h1").unwrap();
        let cfg = parse_config(&serde_json::to_vec(&fx.cfg).unwrap()).unwrap();
        let spec = cfg.populations.iter().find(|p| p.id == "T.N").unwrap();
        let rows: Vec<Row> = parse_rows(&std::fs::read_to_string(&spec.rows).unwrap()).unwrap();
        let humans: Vec<usize> = (0..rows.len())
            .filter(|&i| rows[i].primary && rows[i].origin == 0)
            .collect();
        let w = balanced_weights(&rows, &humans);
        let loss = |name: &str, seed: u64| -> Vec<f64> {
            let s = cfg
                .systems
                .iter()
                .find(|s| s.population == "T.N" && s.name == name && s.seed == seed)
                .unwrap();
            let p = parse_predictions(&std::fs::read_to_string(&s.predictions).unwrap()).unwrap();
            humans
                .iter()
                .map(|&i| score_doc(&rows[i], p.get(&rows[i].id)).0)
                .collect()
        };
        let (a, b) = (loss("Cand.u", 17), loss("R0.u", 17));
        let d: Vec<f64> = a.iter().zip(&b).map(|(x, y)| x - y).collect();
        let v = group_values(&rows, &humans, &humans, &w, &d);
        let (lo, hi) = bootstrap_interval(&v).unwrap();
        let g = &gate(&out, "T3c.T")["levels"]["artifact"];
        assert_eq!(
            f(&g["point"]).to_bits(),
            (v.iter().sum::<f64>() / v.len() as f64).to_bits()
        );
        assert_eq!(f(&g["lower"]).to_bits(), lo.to_bits());
        assert_eq!(f(&g["upper"]).to_bits(), hi.to_bits());
        // The artifact margin is sqrt(2) m_H; the recipe margin m_H.
        let rec = &gate(&out, "T3c.T")["levels"];
        assert_eq!(
            f(&rec["artifact"]["margin"]).to_bits(),
            (std::f64::consts::SQRT_2 * f(&rec["recipe"]["margin"])).to_bits()
        );
        // The recipe comparator is the named recipe system.
        assert_eq!(rec["recipe"]["terms"][0]["comparator"], "Faux.u");
        assert_eq!(rec["artifact"]["terms"][0]["comparator"], "R0.u");
    }

    #[test]
    fn r2_eval_equality_mismatch_blocks_every_gate_and_blend() {
        let fx = fixture();
        let (p, mut e) = release_eval(&fx, "T.N", &[]);
        let ptr = "/systems/Cand.p/seeds/17/balanced_involvement_brier";
        let x = f(e.pointer(ptr).unwrap());
        *e.pointer_mut(ptr).unwrap() = json!(f64::from_bits(x.to_bits() + 1));
        std::fs::write(&p, serde_json::to_vec(&e).unwrap()).unwrap();
        let mut cfg = fx.cfg.clone();
        cfg["release_evaluations"] = json!({"T.N": p});
        let out = run_value(&fx, &cfg, "ulp").unwrap();
        assert_eq!(out["per_seed_equality"]["pass"], false);
        let mm = out["per_seed_equality"]["mismatches"].as_array().unwrap();
        assert_eq!(mm.len(), 1);
        assert_eq!(mm[0]["value"], "/balanced_involvement_brier");
        for g in out["gates"].as_array().unwrap() {
            assert_eq!(g["status"], "not_evaluated");
        }
        for b in ["Cand", "Recal"] {
            assert_eq!(out["blends"][b]["status"], "not_evaluated", "{b}");
            assert!(out["blends"][b]["seed_mean"].is_null());
        }
    }

    #[test]
    fn r2_eval_release_evaluation_inputs_must_be_b10s() {
        // Each tampered input field is found and blocks every gate.
        let fx = fixture();
        type Tamper = (&'static str, fn(&mut Value));
        let tamper: [Tamper; 5] = [
            ("pred:R0.p:17", |e| {
                e["inputs"]["files"]["pred:R0.p:17"]["sha256"] = json!("00")
            }),
            ("split", |e| e["split"] = json!(C_SPLIT)),
            ("rows_jsonl_sha256", |e| {
                e["inputs"]["rows_jsonl_sha256"] = json!("00")
            }),
            ("ops:Cand.p:17", |e| {
                e["inputs"]["files"]["ops:Cand.p:17"]["sha256"] = json!("00")
            }),
            ("ops:Cand.p:29", |e| {
                e["inputs"]["files"]
                    .as_object_mut()
                    .unwrap()
                    .remove("ops:Cand.p:29");
            }),
        ];
        for (k, (field, t)) in tamper.iter().enumerate() {
            let (p, mut e) = release_eval(&fx, "T.N", &[]);
            t(&mut e);
            let p2 = p.with_file_name(format!("tampered-{k}.json"));
            std::fs::write(&p2, serde_json::to_vec(&e).unwrap()).unwrap();
            let mut cfg = fx.cfg.clone();
            cfg["release_evaluations"] = json!({"T.N": p2});
            let out = run_value(&fx, &cfg, &format!("tamper{k}")).unwrap();
            let eq = &out["per_seed_equality"];
            assert_eq!(eq["pass"], false, "{field}");
            assert_eq!(eq["errors"][0]["fields"], json!([field]), "{field}");
            assert!(eq["mismatches"].as_array().unwrap().is_empty(), "{field}");
            for g in out["gates"].as_array().unwrap() {
                assert_eq!(g["status"], "not_evaluated", "{field}");
            }
        }
    }

    #[test]
    fn r2_eval_margins_are_written_before_candidates_are_read() {
        let fx = fixture();
        let mut cfg = fx.cfg.clone();
        let good = cfg["systems"].clone();
        for s in cfg["systems"].as_array_mut().unwrap() {
            if s["name"] == "Cand.p" && s["seed"] == 41 && s["population"] == "T.N" {
                s["predictions"] = json!(fx.dir.path().join("absent.jsonl"));
            }
        }
        let e = err(run_value(&fx, &cfg, "first"));
        assert!(e.contains("absent.jsonl"));
        let mpath = fx.dir.path().join("first-margins.json");
        let m: Value = serde_json::from_slice(&std::fs::read(&mpath).unwrap()).unwrap();
        assert!(!fx.dir.path().join("first-out.json").exists());
        assert_eq!(m["schema"], MARGINS_SCHEMA);
        for i in m["comparator_inputs"].as_array().unwrap() {
            assert!(
                fx.cfg["comparators"]
                    .as_array()
                    .unwrap()
                    .contains(&i["system"])
            );
        }
        // The margins file records the gate candidates, none of them a comparator.
        let cands: Vec<&str> = m["gate_candidates"]
            .as_array()
            .unwrap()
            .iter()
            .map(|x| x.as_str().unwrap())
            .collect();
        assert_eq!(cands, ["Cand.m", "Cand.p", "Cand.t", "Cand.u"]);
        assert_eq!(m["null_check"], false);
        assert_eq!(m["candidates_among_comparators"], json!([]));
        // The full run recomputes byte-identical margins and accepts the file.
        cfg["systems"] = good;
        let out = run_value(&fx, &cfg, "first").unwrap();
        assert_eq!(
            out["margins"]["sha256"],
            sha256_hex(&std::fs::read(&mpath).unwrap()).as_str()
        );
        // Different candidate outputs leave the margins unchanged.
        let mut cfg2 = cfg.clone();
        for s in cfg2["systems"].as_array_mut().unwrap() {
            if s["name"] == "Cand.p" && s["seed"] == 41 {
                let alt = s["predictions"]
                    .as_str()
                    .unwrap()
                    .replace("-41.jsonl", "-29.jsonl");
                s["predictions"] = json!(alt);
            }
        }
        let p2 = fx.dir.path().join("second-margins.json");
        std::fs::copy(&mpath, &p2).unwrap();
        run_value(&fx, &cfg2, "second").unwrap();
        // A margins file that differs is refused.
        let p3 = fx.dir.path().join("third-margins.json");
        let mut b = std::fs::read(&mpath).unwrap();
        b.push(b' ');
        std::fs::write(&p3, b).unwrap();
        assert!(err(run_value(&fx, &cfg, "third")).contains("differs"));
        // A margin from a system outside the comparators is refused.
        let mut cfg4 = fx.cfg.clone();
        cfg4["margins"][0]["system"] = json!("Cand.u");
        assert!(err(run_value(&fx, &cfg4, "fourth")).contains("not a comparator"));
    }

    #[test]
    fn r2_eval_candidates_are_never_comparators_outside_the_null_check() {
        let fx = fixture();
        // Listing a gate candidate as a comparator would read it before the margins.
        for cand in ["Cand.p", "Cand.t", "Cand.m"] {
            let mut cfg = fx.cfg.clone();
            cfg["comparators"].as_array_mut().unwrap().push(json!(cand));
            assert!(
                err(run_value(&fx, &cfg, &format!("cand-{cand}")))
                    .contains("is a gate candidate and is listed as a comparator"),
                "{cand}"
            );
            assert!(!fx.dir.path().join(format!("cand-{cand}-margins.json")).exists());
        }
        // The null check (Faux 29 as the candidate) may take margins from its own seeds.
        let mut cfg = fx.cfg.clone();
        cfg["null_check"] = json!(true);
        cfg["released_seed"] = json!(29);
        cfg["gates"] = json!([
            {"kind": "paired", "id": "T3c.null", "statistic": "human_brier_balanced", "terms": [{"population": "T.N"}],
             "candidate": "Faux.u", "comparator": "R0.u", "levels": ["artifact"], "margin": {"margin": "m_H.T"}}]);
        cfg["blends"] = json!([]);
        let out = run_value(&fx, &cfg, "null").unwrap();
        let m: Value =
            serde_json::from_slice(&std::fs::read(fx.dir.path().join("null-margins.json")).unwrap())
                .unwrap();
        assert_eq!(m["null_check"], true);
        assert_eq!(m["candidates_among_comparators"], json!(["Faux.u"]));
        assert_eq!(out["margins"]["null_check"], true);
        let g = gate(&out, "T3c.null");
        assert_eq!(g["levels"]["artifact"]["terms"][0]["seeds_candidate"], json!([29]));
        assert!(g["status"] == "pass" || g["status"] == "fail");
    }

    #[test]
    fn r2_eval_margin_values_and_faults() {
        let fx = fixture();
        let out = run_value(&fx, &fx.cfg, "mv").unwrap();
        let m: Value =
            serde_json::from_slice(&std::fs::read(fx.dir.path().join("mv-margins.json")).unwrap())
                .unwrap();
        let mh = &m["margins"]["m_H.T"];
        let per: Vec<f64> = RECIPE_SEEDS
            .iter()
            .map(|s| f(&out["systems"]["T.N"]["Faux.u"][s.to_string()]["bound"]["human_mean_brier_balanced"]))
            .collect();
        for (k, s) in RECIPE_SEEDS.iter().enumerate() {
            assert_eq!(
                f(&mh["per_seed"][s.to_string()]).to_bits(),
                per[k].to_bits()
            );
        }
        let sd = sample_sd(&per).unwrap();
        assert_eq!(f(&mh["value"]).to_bits(), sd.to_bits());
        assert_eq!(
            f(&mh["artifact_value"]).to_bits(),
            (std::f64::consts::SQRT_2 * sd).to_bits()
        );
        assert!(mh["fault"].is_null());
        let w = &m["margins"]["wT2.T1a"];
        let b0 = f(&out["systems"]["T.N"]["R0.p"]["17"]["gate_weight_brier"]);
        assert_eq!(f(&w["value"]).to_bits(), (SIGMA_M * b0).to_bits());
        assert_eq!(w["value"], w["artifact_value"]);
        assert_eq!(
            f(&gate(&out, "T1a")["levels"]["artifact"]["margin"]).to_bits(),
            (SIGMA_M * b0).to_bits()
        );
        // Two comparator seeds: a margin-rule fault (item 1.10 (a)), and the gate is not evaluated.
        let mut cfg = fx.cfg.clone();
        cfg["systems"]
            .as_array_mut()
            .unwrap()
            .retain(|s| !(s["name"] == "Faux.u" && s["seed"] == 41));
        // (A recipe comparator with two seeds is itself refused, so T3c keeps its artifact level here.)
        set_gate(&mut cfg, "T3c.T", |g| g["levels"] = json!(["artifact"]));
        let out = run_value(&fx, &cfg, "fault").unwrap();
        let m: Value = serde_json::from_slice(
            &std::fs::read(fx.dir.path().join("fault-margins.json")).unwrap(),
        )
        .unwrap();
        assert!(
            m["margins"]["m_H.T"]["fault"]
                .as_str()
                .unwrap()
                .contains("item 1.10")
        );
        let g = gate(&out, "T3c.T");
        assert_eq!(g["status"], "not_evaluated");
        assert!(g["reasons"][0].as_str().unwrap().contains("m_H.T"));
    }

    #[test]
    fn r2_eval_margin_comparison() {
        assert!(margin_pass(0.0, 0.0, false));
        assert!(!margin_pass(0.0, 0.0, true));
        assert!(margin_pass(-1e-12, 0.0, true));
        assert!(!margin_pass(0.0101, V12_DELTA, false));
    }

    #[test]
    fn r2_eval_tie_interval_includes_a_zero_endpoint() {
        // Candidate and comparator identical: every draw is 0, both endpoints are 0, and the
        // interval includes zero (§9.2 step 7).
        let fx = fixture();
        let out = run_value(&fx, &fx.cfg, "tie0").unwrap();
        let g = gate(&out, "tie0");
        assert_eq!(g["status"], "reported");
        for lvl in ["artifact", "recipe"] {
            let r = &g["levels"][lvl];
            assert_eq!(f(&r["point"]), 0.0, "{lvl}");
            assert_eq!(f(&r["lower"]), 0.0, "{lvl}");
            assert_eq!(f(&r["upper"]), 0.0, "{lvl}");
            assert_eq!(r["interval_includes_zero"], true, "{lvl}");
        }
    }

    #[test]
    fn r2_eval_t8a_branch_table() {
        // §12.3's table (V), exact on the decimal value of s.
        for (s, b, n, short) in [
            (0.035, T8aBranch::WHalf, 8, false),
            (0.07, T8aBranch::WHalf, 32, false),
            (0.095, T8aBranch::WHalf, 58, false),
            (0.10, T8aBranch::Delta, 16, false),
            (0.14, T8aBranch::Delta, 32, false),
            (0.19, T8aBranch::Delta, 58, false),
            (0.2, T8aBranch::Delta, 58, true),
            (0.0, T8aBranch::WHalf, 1, false),
            (0.01, T8aBranch::WHalf, 1, false),
            (0.0125, T8aBranch::WHalf, 1, false),
            (0.0126, T8aBranch::WHalf, 2, false),
            // "If s > 0.19, B_M = 58, and the shortfall is recorded": on s's decimal value,
            // also where ceil(1600 s^2) is still 58.
            (0.1901, T8aBranch::Delta, 58, true),
            (0.1903, T8aBranch::Delta, 58, true),
            (0.1904, T8aBranch::Delta, 58, true),
            (0.19000000000000003, T8aBranch::Delta, 58, true),
            (0.18999999999999997, T8aBranch::Delta, 58, false),
            (1.5, T8aBranch::Delta, 58, true),
        ] {
            assert_eq!(t8a_branch(s).unwrap(), (b, n, short), "s = {s}");
        }
        // The w/2 limit is s with 6400 s^2 <= 58 exactly; 0.0951 gives 57.88..., 0.0952 gives 58.0...6.
        assert_eq!(t8a_branch(0.0951).unwrap().0, T8aBranch::WHalf);
        assert_eq!(t8a_branch(0.0952).unwrap(), (T8aBranch::Delta, 15, false));
        assert!(t8a_branch(-0.1).is_err() && t8a_branch(f64::NAN).is_err());
        assert!(t8a_pass(T8aBranch::WHalf, 0.01, SIGMA_M));
        assert!(!t8a_pass(T8aBranch::WHalf, -0.1, 0.0351));
        assert!(!t8a_pass(T8aBranch::Delta, 0.001, 0.05));
        assert!(t8a_pass(T8aBranch::Delta, 0.0, DELTA_M));
        assert!(!t8a_pass(T8aBranch::Delta, -0.01, 0.0701));
    }

    /// Per-row (loss, failed) from a predictions file, by the bound scoring rule, computed here.
    fn losses(path: &Path, rows: &[Value]) -> Vec<f64> {
        let mut p: BTreeMap<String, Value> = BTreeMap::new();
        for l in std::fs::read_to_string(path).unwrap().lines() {
            let v: Value = serde_json::from_str(l).unwrap();
            p.insert(v["id"].as_str().unwrap().to_string(), v);
        }
        rows.iter()
            .map(|r| {
                let y = if r["origin"] == "human_only" { 0.0 } else { 1.0 };
                match p.get(r["id"].as_str().unwrap()) {
                    Some(v) if v.get("status").is_none() => {
                        let inv = (f(&v["p_model_only"]) + f(&v["p_mixed"])).min(1.0);
                        (inv - y) * (inv - y)
                    }
                    _ => 1.0,
                }
            })
            .collect()
    }

    /// `r2_t8a_v2` computed here from the raw mock rows: W_A8 weights from the row field,
    /// ledger units from the unit field, the seed-mean losses, the ratio and its bootstrap.
    fn t8a_here(fx: &Fixture, cand: &str, cs: &[u64], comp: &str, rs: &[u64]) -> (f64, f64, f64, usize) {
        let rows: Vec<Value> = std::fs::read_to_string(fx.dir.path().join("mock.jsonl"))
            .unwrap()
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();
        let raw: Vec<f64> = rows.iter().map(|r| f(&r["w_a8"])).collect();
        let tot: f64 = raw.iter().sum();
        let w: Vec<f64> = raw.iter().map(|x| x / tot).collect();
        let mean = |name: &str, seeds: &[u64]| -> Vec<f64> {
            let per: Vec<Vec<f64>> = seeds
                .iter()
                .map(|s| losses(&fx.dir.path().join(format!("pred-mock-{name}-{s}.jsonl")), &rows))
                .collect();
            (0..rows.len())
                .map(|i| per.iter().map(|v| v[i]).sum::<f64>() / per.len() as f64)
                .collect()
        };
        let (lc, lr) = (mean(cand, cs), mean(comp, rs));
        let mut u: BTreeMap<String, (f64, f64)> = BTreeMap::new();
        for i in 0..rows.len() {
            let e = u
                .entry(rows[i]["ledger_unit"].as_str().unwrap().to_string())
                .or_default();
            e.0 += w[i] * (lc[i] - lr[i]);
            e.1 += w[i] * lr[i];
        }
        let (d, r): (Vec<f64>, Vec<f64>) = u.into_values().unzip();
        let point = d.iter().sum::<f64>() / r.iter().sum::<f64>();
        let n = d.len();
        let mut seed = 20261026u64;
        let mut draws = vec![];
        for _ in 0..1000 {
            let (mut a, mut b) = (0.0, 0.0);
            for _ in 0..n {
                seed ^= seed << 13;
                seed ^= seed >> 7;
                seed ^= seed << 17;
                let j = seed as usize % n;
                a += d[j];
                b += r[j];
            }
            draws.push(a / b);
        }
        draws.sort_by(f64::total_cmp);
        (point, draws[24], draws[974], n)
    }

    #[test]
    fn r2_eval_t8a_gate() {
        let fx = fixture();
        let out = run_value(&fx, &fx.cfg, "t8a").unwrap();
        let g = gate(&out, "T8a");
        // Computed here from the raw rows (W_A8 field weights, ledger units across groups).
        for (lvl, cs) in [("artifact", &[17u64][..]), ("recipe", &RECIPE_SEEDS[..])] {
            let a = &g["levels"][lvl];
            let (point, lo, hi, n) = t8a_here(&fx, "Cand.t", cs, "R0.t", &[17]);
            assert_eq!(a["ledger_units"], n, "{lvl}");
            assert_eq!(n, 12);
            assert!((f(&a["point"]) - point).abs() < 1e-12, "{lvl}");
            assert!((f(&a["lower"]) - lo).abs() < 1e-12, "{lvl}");
            assert!((f(&a["upper"]) - hi).abs() < 1e-12, "{lvl}");
            assert_eq!(a["pass"], json!(hi <= SIGMA_M), "{lvl}");
        }
        assert_eq!(g["reserve_check"]["realised_batches"], 2);
        assert_eq!(g["reserve_check"]["batches_short"], 0);
        assert_eq!(g["sealed"]["s_above_0_19_shortfall"], false);
        // The recipe comparator override: R0.t3's seed mean, not R0.t.
        let rc = gate(&out, "T8a.rc");
        let r = &rc["levels"]["recipe"];
        assert_eq!(r["comparator"], "R0.t3");
        assert_eq!(r["seeds_comparator"], json!(RECIPE_SEEDS));
        let (point, _, hi, _) = t8a_here(&fx, "Cand.t", &RECIPE_SEEDS, "R0.t3", &RECIPE_SEEDS);
        assert!((f(&r["point"]) - point).abs() < 1e-12);
        assert!((f(&r["upper"]) - hi).abs() < 1e-12);
        let (p0, _, _, _) = t8a_here(&fx, "Cand.t", &RECIPE_SEEDS, "R0.t", &[17]);
        assert!((point - p0).abs() > 1e-6);
        // A sealed branch that does not follow from s is refused.
        let mut cfg = fx.cfg.clone();
        set_gate(&mut cfg, "T8a", |t| t["sealed"]["b_m"] = json!(3));
        assert!(run_value(&fx, &cfg, "t8a-b").is_err());
    }

    #[test]
    fn r2_eval_t8a_realised_mock_shortfall() {
        // Amendment 8.1: the reserve check's B_M' is what the mock must hold.
        let fx = fixture();
        let d = fx.dir.path().to_path_buf();
        let one_batch = |fx: &Fixture| {
            let mut rows = mock_rows();
            for r in &mut rows {
                r["batch"] = json!(0);
            }
            write_lines(&fx.dir.path().join("mock.jsonl"), &rows);
        };
        // (a) A recorded shortfall: B_M' = 1 of B_M = 2, the mock built at 1 batch: evaluated and reported.
        one_batch(&fx);
        let mut cfg = fx.cfg.clone();
        let rc = reserve_check(&d, "short", 2, 1, &["wikipedia"]);
        set_gate(&mut cfg, "T8a", |t| t["sealed"]["reserve_check"] = rc.clone());
        set_gate(&mut cfg, "T8a.rc", |t| t["sealed"]["reserve_check"] = rc.clone());
        let out = run_value(&fx, &cfg, "short").unwrap();
        let g = gate(&out, "T8a");
        assert!(g["status"] == "pass" || g["status"] == "fail", "{g}");
        assert_eq!(g["reserve_check"]["b_m"], 2);
        assert_eq!(g["reserve_check"]["b_m_prime"], 1);
        assert_eq!(g["reserve_check"]["batches_short"], 1);
        assert_eq!(g["reserve_check"]["binding_domains"], json!(["wikipedia"]));
        assert_eq!(g["reserve_check"]["realised_batches"], 1);
        assert_eq!(g["sealed"]["b_m"], 2);
        assert_eq!(g["criterion"], "draws[974] <= sigma_m = 0.035");
        // (b) No shortfall recorded but the mock holds 1 batch: not evaluated, the run completes.
        let out = run_value(&fx, &fx.cfg, "nobm").unwrap();
        let g = gate(&out, "T8a");
        assert_eq!(g["status"], "not_evaluated");
        assert!(g["reasons"][0].as_str().unwrap().contains("not the reserve check's B_M' 2"));
        assert!(d.join("nobm-out.json").exists());
        assert_ne!(gate(&out, "T1a")["status"], "not_evaluated");
        // (c) No B >= 1 is feasible: T8a is not evaluated, the run completes.
        let mut cfg = fx.cfg.clone();
        let rc0 = reserve_check(&d, "none", 2, 0, &["wikipedia", "reddit"]);
        set_gate(&mut cfg, "T8a", |t| t["sealed"]["reserve_check"] = rc0.clone());
        let out = run_value(&fx, &cfg, "none").unwrap();
        let g = gate(&out, "T8a");
        assert_eq!(g["status"], "not_evaluated");
        assert!(g["reasons"][0].as_str().unwrap().contains("no B >= 1 is feasible"));
        assert_eq!(g["reserve_check"]["batches_short"], 2);
        assert!(d.join("none-out.json").exists());
        // (d) A record whose bytes are not the logged digest is refused.
        let mut cfg = fx.cfg.clone();
        let mut bad = reserve_check(&d, "bad", 2, 1, &["wikipedia"]);
        bad["test2_mock_reserve_check_sha256"] = json!("0".repeat(64));
        set_gate(&mut cfg, "T8a", |t| t["sealed"]["reserve_check"] = bad.clone());
        assert!(err(run_value(&fx, &cfg, "bad")).contains("not the logged test2_mock_reserve_check_sha256"));
        // (e) Inconsistent records and a missing batch field are refused before any read.
        for (tag, b_m, b_m_prime, binding) in [
            ("rc-bm", 3u64, 1u64, &["wikipedia"][..]),
            ("rc-over", 2, 3, &["wikipedia"][..]),
            ("rc-nobind", 2, 1, &[][..]),
            ("rc-bind", 2, 2, &["wikipedia"][..]),
        ] {
            let mut cfg = fx.cfg.clone();
            let r = reserve_check(&d, tag, b_m, b_m_prime, binding);
            set_gate(&mut cfg, "T8a", |t| t["sealed"]["reserve_check"] = r.clone());
            assert!(run_value(&fx, &cfg, tag).is_err(), "{tag}");
            assert!(!d.join(format!("{tag}-margins.json")).exists(), "{tag}");
        }
        let mut cfg = fx.cfg.clone();
        set_gate(&mut cfg, "T8a", |t| {
            t.as_object_mut().unwrap().remove("batch_field");
        });
        assert!(parse_config(&serde_json::to_vec(&cfg).unwrap()).is_err());
    }

    #[test]
    fn r2_eval_completion_gate() {
        // T1d: Cand.p seed 29 fails one T.N document and seed 41 misses a human in both forms.
        let fx = fixture();
        let out = run_value(&fx, &fx.cfg, "t1d").unwrap();
        let g = gate(&out, "T1d");
        assert_eq!(g["status"], "fail");
        assert_eq!(g["levels"]["artifact"]["pass"], true);
        assert_eq!(g["levels"]["recipe"]["pass"], false);
        let seeds = g["levels"]["recipe"]["seeds"].as_array().unwrap();
        let get = |pop: &str, s: u64| seeds.iter().find(|x| x["population"] == pop && x["seed"] == s).unwrap();
        assert_eq!((get("T.N", 29)["execution_failures"].as_u64(), get("T.N", 29)["missing_predictions"].as_u64()), (Some(1), Some(0)));
        assert_eq!((get("T.N", 41)["execution_failures"].as_u64(), get("T.N", 41)["missing_predictions"].as_u64()), (Some(0), Some(1)));
        assert_eq!(get("T.R", 41)["missing_predictions"], 1);
        // A missing prediction is not a completed result.
        assert_eq!(get("T.R", 41)["pass"], false);
        assert_eq!(get("T.N", 41)["pass"], false);
        assert_eq!(get("T.R", 29)["pass"], true);
        assert_eq!(get("T.N", 17)["pass"], true);
        // A complete system passes at both levels.
        let mut cfg = fx.cfg.clone();
        set_gate(&mut cfg, "T1d", |t| {
            t["system"] = json!("Cand.u");
            t["populations"] = json!(["T.N"]);
        });
        let out = run_value(&fx, &cfg, "t1d-ok").unwrap();
        assert_eq!(gate(&out, "T1d")["status"], "pass");
    }

    #[test]
    fn r2_eval_exact_paired_test() {
        // P(Bin(n, 1/2) >= b) against 1/20, decided exactly.
        assert!(!exact_paired_fails(0, 0).0);
        assert!(!exact_paired_fails(3, 0).0); // 1/8
        assert!(!exact_paired_fails(4, 0).0); // 1/16 = 0.0625
        assert!(exact_paired_fails(5, 0).0); // 1/32
        assert!(!exact_paired_fails(6, 1).0); // 8/128 = 0.0625
        assert!(exact_paired_fails(7, 1).0); // 9/256
        assert!(!exact_paired_fails(0, 9).0);
        let (fails, p) = exact_paired_fails(7, 1);
        assert!(fails && (p - 9.0 / 256.0).abs() < 1e-15);
        // Large n: the exact decision agrees with the f64 tail away from the boundary.
        for (b, c) in [(120u64, 80u64), (110, 90), (1500, 1300), (1460, 1340)] {
            let (fails, p) = exact_paired_fails(b, c);
            assert_eq!(fails, p < 0.05, "b {b} c {c} p {p}");
        }
        // §11.5 T3b (V): false fail 1.4% at 2.8 expected flags each; power 0.40 against a 3x rate.
        let pois = |l: f64, k: u64| {
            (-l + k as f64 * l.ln() - (1..=k).map(|i| (i as f64).ln()).sum::<f64>()).exp()
        };
        let rate = |lb: f64, lc: f64| -> f64 {
            let mut s = 0.0;
            for b in 0..60 {
                for c in 0..60 {
                    if exact_paired_fails(b, c).0 {
                        s += pois(lb, b) * pois(lc, c);
                    }
                }
            }
            s
        };
        assert_eq!((rate(2.8, 2.8) * 1000.0).round() / 1000.0, 0.014);
        assert_eq!((rate(8.4, 2.8) * 100.0).round() / 100.0, 0.40);
    }

    /// 2,800 humans in 140 groups plus one mixed positive per group, block T. Cand.x and
    /// Recal.x flags are designed per human (flag = 1 − p_human), with exact ties at the
    /// declared thresholds (each threshold is 1 − p of a tied human, computed the same way).
    fn flags_fixture() -> Fixture {
        let dir = tempfile::tempdir().unwrap();
        let d = dir.path();
        let mut rows = vec![];
        for i in 0..2800usize {
            let g = i / 20;
            let dom = if g % 2 == 0 { "wikipedia" } else { "reddit" };
            rows.push(row(&format!("h-{i}"), T_SPLIT, "human_only", &format!("fg{g}"), dom, "primary"));
        }
        for g in 0..140usize {
            let dom = if g % 2 == 0 { "wikipedia" } else { "reddit" };
            rows.push(row(&format!("p-{g}"), T_SPLIT, "mixed", &format!("fg{g}"), dom, "primary"));
        }
        for r in &mut rows {
            r["block"] = json!("T");
        }
        let p = d.join("tf.jsonl");
        write_lines(&p, &rows);
        let (cp_tie, ex_tie, rc_tie) = (0.25, 0.05, 0.1);
        // Cand.x: humans 0..k flagged at the 1% point (h0 tied), humans 1..=e tied at the 0.1% point.
        let cand = |seed: u64, i: usize| -> f64 {
            let (e, k) = match seed {
                17 => (5, 56),
                29 => (7, 56),
                _ => (5, 57),
            };
            if i == 0 {
                cp_tie
            } else if (1..=e).contains(&i) {
                ex_tie
            } else if i < k {
                0.2
            } else if i == 101 || i == 102 {
                0.4
            } else {
                0.9
            }
        };
        // Recal.x: h100 tied at its 0.1% point; h101 and h102 above it (not at seed 29).
        let recal = |seed: u64, i: usize| -> f64 {
            if i == 100 {
                rc_tie
            } else if (i == 101 || i == 102) && seed != 29 {
                0.08
            } else {
                0.9
            }
        };
        let mut systems = vec![];
        for (name, ph, t1, t01) in [
            ("Cand.x", &cand as &dyn Fn(u64, usize) -> f64, 1.0 - cp_tie, 1.0 - ex_tie),
            ("Recal.x", &recal as &dyn Fn(u64, usize) -> f64, 1.0 - cp_tie, 1.0 - rc_tie),
        ] {
            for seed in RECIPE_SEEDS {
                let lines: Vec<Value> = rows
                    .iter()
                    .map(|r| {
                        let id = r["id"].as_str().unwrap();
                        let h = match id.strip_prefix("h-") {
                            Some(i) => ph(seed, i.parse().unwrap()),
                            None => 0.3,
                        };
                        let pm = 0.6 * (1.0 - h);
                        json!({"id": id, "p_human": h, "p_model_only": pm, "p_mixed": (1.0 - h) - pm, "share": pm})
                    })
                    .collect();
                let pp = d.join(format!("pred-{name}-{seed}.jsonl"));
                write_lines(&pp, &lines);
                let op = d.join(format!("ops-{name}-{seed}.json"));
                std::fs::write(&op, json!({"0.01": t1, "0.001": t01}).to_string()).unwrap();
                systems.push(json!({"name": name, "seed": seed, "population": "T.F", "predictions": pp, "ops": op}));
            }
        }
        let cfg = json!({
            "schema": CONFIG_SCHEMA,
            "populations": [{"id": "T.F", "block": "T", "form": "N", "split": T_SPLIT, "rows": p, "weights": {"rule": "balanced"}}],
            "systems": systems,
            "gates": [
                {"kind": "cp_flags", "id": "T3a.F", "population": "T.F", "system": "Cand.x", "point": "0.01", "levels": ["artifact", "recipe"]},
                {"kind": "cp_flags", "id": "T3a.F17", "population": "T.F", "system": "Cand.x", "point": "0.01", "levels": ["artifact"]},
                {"kind": "exact_paired", "id": "T3b.F", "population": "T.F", "candidate": "Cand.x", "comparator": "Recal.x", "point": "0.001",
                 "levels": ["artifact", "recipe"]},
                {"kind": "exact_paired", "id": "T3b.F17", "population": "T.F", "candidate": "Cand.x", "comparator": "Recal.x", "point": "0.001",
                 "levels": ["artifact"]}
            ]
        });
        Fixture { dir, cfg }
    }

    #[test]
    fn r2_eval_cp_and_exact_gates_at_size_with_ties() {
        let fx = flags_fixture();
        let out = run_value(&fx, &fx.cfg, "fl").unwrap();
        assert_eq!(out["per_seed_equality"]["pass"], true);
        // T3a: §12.1's limit 56 of 2,800; ties at the threshold count as flagged.
        let c = gate(&out, "T3a.F");
        assert_eq!(c["flag_limit"], 56);
        let want = [(17u64, 56u64, true), (29, 56, true), (41, 57, false)];
        let seeds = c["levels"]["recipe"]["seeds"].as_array().unwrap();
        assert_eq!(seeds.len(), 3);
        for (s, (seed, k, ok)) in seeds.iter().zip(want) {
            assert_eq!(s["seed"], seed);
            assert_eq!(s["flags"], k, "seed {seed}");
            assert_eq!(s["humans"], 2800);
            assert_eq!(s["pass"], ok, "seed {seed}");
            // The gate's count is the operating-point count compared with the bound evaluator's.
            assert_eq!(
                s["flags"],
                out["systems"]["T.F"]["Cand.x"][seed.to_string()]["bound"]["operating_points"]["0.01"]["human_fpr"]["count"]
            );
        }
        assert_eq!(c["levels"]["artifact"]["pass"], true);
        assert_eq!(c["levels"]["recipe"]["pass"], false);
        assert_eq!(c["levels"]["recipe"]["max_flags"], 57);
        assert_eq!(c["status"], "fail");
        assert_eq!(gate(&out, "T3a.F17")["status"], "pass");
        // T3b: b and c at each system's own threshold, ties flagged; known values.
        let e = gate(&out, "T3b.F");
        let pairs = e["levels"]["recipe"]["pairs"].as_array().unwrap();
        let want = [(17u64, 5u64, 3u64, false, 93.0 / 256.0), (29, 7, 1, true, 9.0 / 256.0), (41, 5, 3, false, 93.0 / 256.0)];
        for (p, (seed, b, cc, fails, tail)) in pairs.iter().zip(want) {
            assert_eq!((p["seed_candidate"].as_u64(), p["seed_comparator"].as_u64()), (Some(seed), Some(seed)));
            assert_eq!(p["b_candidate_only"], b, "seed {seed}");
            assert_eq!(p["c_comparator_only"], cc, "seed {seed}");
            assert_eq!(p["fails"], fails, "seed {seed}");
            assert!((f(&p["p_upper_tail"]) - tail).abs() < 1e-12, "seed {seed}");
        }
        assert_eq!(e["levels"]["artifact"]["pairs"][0]["b_candidate_only"], 5);
        assert_eq!(e["levels"]["artifact"]["pass"], true);
        assert_eq!(e["levels"]["recipe"]["pass"], false);
        assert_eq!(e["status"], "fail");
        assert_eq!(gate(&out, "T3b.F17")["status"], "pass");
    }

    #[test]
    fn r2_eval_exact_and_cp_gates() {
        let fx = fixture();
        let out = run_value(&fx, &fx.cfg, "flags").unwrap();
        let g = gate(&out, "T3b");
        let pairs = g["levels"]["recipe"]["pairs"].as_array().unwrap();
        assert_eq!(pairs.len(), 3);
        for p in pairs {
            assert_eq!(p["seed_candidate"], p["seed_comparator"]);
        }
        // The CP rule: §12.1's limits (V), 56 of 2,800 and 29 of 1,600.
        assert_eq!(max_flags(2800, CP_LEVEL, CP_BOUND), Some(56));
        assert_eq!(max_flags(1600, CP_LEVEL, CP_BOUND), Some(29));
        assert!(
            clopper_pearson_upper(56, 2800, CP_LEVEL) <= CP_BOUND
                && clopper_pearson_upper(57, 2800, CP_LEVEL) > CP_BOUND
        );
        let c = gate(&out, "T3a.T");
        // 48 humans: even 0 flags has a CP bound above 2.5%, so there is no limit and every seed fails.
        assert_eq!(c["flag_limit"], Value::Null);
        assert_eq!(c["status"], "fail");
        // The missing human of Cand.p seed 41 is scored worst case: flagged at every threshold.
        let s41 = c["levels"]["recipe"]["seeds"]
            .as_array()
            .unwrap()
            .iter()
            .find(|s| s["seed"] == 41)
            .unwrap();
        assert!(s41["flags"].as_u64().unwrap() >= 1);
        // Declared operating points are required.
        let mut cfg = fx.cfg.clone();
        set_gate(&mut cfg, "T3a.T", |t| t["system"] = json!("Cand.u"));
        assert!(err(run_value(&fx, &cfg, "noops")).contains("CAL-declared operating point"));
    }

    #[test]
    fn r2_eval_bootstrap_single_block_is_bound() {
        let v: Vec<f64> = (0..37).map(|i| unif(&format!("v{i}")) - 0.5).collect();
        let d = stratified_bootstrap(&[&v]).unwrap();
        let (lo, hi) = bootstrap_interval(&v).unwrap();
        assert_eq!(
            (d[24].to_bits(), d[974].to_bits()),
            (lo.to_bits(), hi.to_bits())
        );
        let n = v.len();
        let b = bootstrap_draws(n, |ix| {
            let mut t = 0.0;
            for &j in ix {
                t += v[j];
            }
            t / n as f64
        })
        .unwrap();
        assert_eq!(
            d.iter().map(|x| x.to_bits()).collect::<Vec<_>>(),
            b.iter().map(|x| x.to_bits()).collect::<Vec<_>>()
        );
        assert_eq!(stratified_bootstrap(&[&v]).unwrap(), d);
        assert!(stratified_bootstrap(&[&v[..1]]).is_none());
    }

    #[test]
    fn r2_eval_bootstrap_block_order() {
        // Two blocks: within each draw, T's indices come first, then C's, from one stream.
        let t: Vec<f64> = (0..5).map(|i| i as f64 * 0.1).collect();
        let c: Vec<f64> = (0..3).map(|i| 1.0 + i as f64).collect();
        let mut seed = 20261026u64;
        let mut next = |n: usize| {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed as usize % n
        };
        let mut want = vec![];
        for _ in 0..1000 {
            let mut a = 0.0;
            for _ in 0..5 {
                a += t[next(5)];
            }
            let mut b = 0.0;
            for _ in 0..3 {
                b += c[next(3)];
            }
            want.push(a / 5.0 + b / 3.0);
        }
        want.sort_by(f64::total_cmp);
        assert_eq!(stratified_bootstrap(&[&t, &c]).unwrap(), want);
        // composite_units: units per block are the union of the terms' units, and the block means sum to the statistic.
        let terms = [
            Term {
                block: Block::T,
                units: vec!["a", "b"],
                weights: vec![0.5, 0.5],
                delta: vec![0.2, -0.1],
                coef: 0.25,
            },
            Term {
                block: Block::T,
                units: vec!["b", "c"],
                weights: vec![0.25, 0.75],
                delta: vec![0.4, 0.0],
                coef: 0.25,
            },
            Term {
                block: Block::C,
                units: vec!["x", "y"],
                weights: vec![0.5, 0.5],
                delta: vec![-0.2, 0.1],
                coef: 0.5,
            },
        ];
        let (point, blocks) = composite_units(&terms);
        assert_eq!(blocks.len(), 2);
        assert_eq!(blocks[0].1.len(), 3);
        let want =
            0.25 * (0.5 * 0.2 - 0.5 * 0.1) + 0.25 * (0.25 * 0.4) + 0.5 * (0.5 * -0.2 + 0.5 * 0.1);
        assert!((point - want).abs() < 1e-15);
    }

    fn parse_c(v: &[Value]) -> Vec<Row> {
        parse_rows(&v.iter().map(|r| r.to_string()).collect::<Vec<_>>().join("\n")).unwrap()
    }

    #[test]
    fn r2_eval_cohort_weights() {
        let t = ShareTable::parse(SHARES).unwrap();
        let rows = parse_c(&c_rows());
        let idx: Vec<usize> = (0..rows.len()).collect();
        let (w, rec) = cohort_weights(&rows, &idx, &t).unwrap();
        assert!((w.iter().sum::<f64>() - 1.0).abs() < 1e-12);
        let mass = |rows: &[Row], w: &[f64], idx: &[usize], f: &dyn Fn(&Row) -> bool| -> f64 {
            idx.iter().zip(w).filter(|(i, _)| f(&rows[**i])).map(|(_, x)| x).sum()
        };
        assert!((mass(&rows, &w, &idx, &|r| r.origin == 0) - 0.20).abs() < 1e-12);
        assert!((mass(&rows, &w, &idx, &|r| r.origin == 1) - 0.25).abs() < 1e-12);
        assert!((mass(&rows, &w, &idx, &|r| r.origin == 2) - 0.55).abs() < 1e-12);
        let present = 26.5 + 51.2516 + 0.6884;
        assert!(
            (mass(&rows, &w, &idx, &|r| r.origin == 0 && r.domain == "news") - 0.20 * 26.5 / present).abs()
                < 1e-12
        );
        // Equal group mass within a cell; equal document mass within a group.
        let a = idx.iter().find(|&&i| rows[i].id == "ch-1-0").unwrap();
        let b = idx.iter().find(|&&i| rows[i].id == "ch-1-1").unwrap();
        assert_eq!(w[*a], w[*b]);
        let g1: f64 = mass(&rows, &w, &idx, &|r| r.origin == 0 && r.group == "cg1.example");
        let g4: f64 = mass(&rows, &w, &idx, &|r| r.origin == 0 && r.group == "cg4.example");
        assert!((g1 - g4).abs() < 1e-15); // both essays humans, 2 and 1 documents
        assert!(rec["absent_origins"].as_array().unwrap().is_empty());
        assert!(rec["absent_cells"].as_array().unwrap().is_empty());
        assert_eq!(rec["share_table_sha256"], SHARES_SHA256);
        // An absent origin hands its share to the others in proportion, and is recorded.
        let no_model: Vec<usize> = idx
            .iter()
            .copied()
            .filter(|&i| rows[i].origin != 1)
            .collect();
        let (w2, rec2) = cohort_weights(&rows, &no_model, &t).unwrap();
        assert!((mass(&rows, &w2, &no_model, &|r| r.origin == 0) - 0.20 / 0.75).abs() < 1e-12);
        assert_eq!(rec2["absent_origins"][0], "model_only");
        // An absent cell: model_only without news renormalises model_only's strata over
        // essays and email (item 15.1), and the cell is recorded.
        let no_mo_news: Vec<usize> = idx
            .iter()
            .copied()
            .filter(|&i| !(rows[i].origin == 1 && rows[i].domain == "news"))
            .collect();
        let (w3, rec3) = cohort_weights(&rows, &no_mo_news, &t).unwrap();
        assert_eq!(rec3["absent_cells"], json!(["model_only|news"]));
        let mo = |s: &str| mass(&rows, &w3, &no_mo_news, &|r| r.origin == 1 && r.domain == s);
        assert!((mo("essays") - 0.25 * 51.2516 / (51.2516 + 0.6884)).abs() < 1e-12);
        assert!((mass(&rows, &w3, &no_mo_news, &|r| r.origin == 0 && r.domain == "news") - 0.20 * 26.5 / present).abs() < 1e-12);
        // Unknown strata and a mixed email encoding are refused.
        let mut bad = c_rows();
        bad[0]["domain"] = json!("poetry");
        assert!(cohort_weights(&parse_c(&bad), &idx, &t).is_err());
        bad[0]["domain"] = json!("press_release");
        assert!(cohort_weights(&parse_c(&bad), &idx, &t).is_err());
    }

    #[test]
    fn r2_eval_share_table_is_the_tracked_v5() {
        assert_eq!(sha256_hex(SHARES), SHARES_SHA256);
        let t = ShareTable::parse(SHARES).unwrap();
        assert_eq!(t.id, SHARES_ID);
        assert_eq!(t.stratum("news"), Some(26.5));
        assert_eq!(t.stratum("essays"), Some(51.2516));
        // The same id with two shares swapped, a re-serialised copy and another id are refused.
        let s = std::str::from_utf8(SHARES).unwrap();
        let swapped = s
            .replace("\"news\": 26.5", "\"news\": X")
            .replace("\"essays\": 51.2516", "\"essays\": 26.5")
            .replace("\"news\": X", "\"news\": 51.2516");
        assert_ne!(swapped, s);
        assert!(ShareTable::parse(swapped.as_bytes()).is_err());
        let v: Value = serde_json::from_slice(SHARES).unwrap();
        assert!(ShareTable::parse(v.to_string().as_bytes()).is_err());
        assert!(ShareTable::parse(s.replace("_v5", "_v4").as_bytes()).is_err());
    }

    #[test]
    fn r2_eval_split_policy() {
        for s in OPEN_SPLITS {
            assert_eq!(split_policy(s).unwrap(), SplitPolicy::Open, "{s}");
        }
        for s in SEALED_SPLITS {
            assert_eq!(split_policy(s).unwrap(), SplitPolicy::Sealed, "{s}");
        }
        for s in REFUSED_SPLITS {
            assert!(split_policy(s).is_err(), "{s}");
        }
        assert!(format!("{:#}", split_policy("dev_select").unwrap_err()).contains("never read"));
        for s in ["", "test2", "dev_iter2", "dev_select2", "devselect2", "cal2", "test2_t2", "Test2_t"] {
            assert!(
                format!("{:#}", split_policy(s).unwrap_err()).contains("closed list"),
                "{s}"
            );
        }
        assert_eq!(split_blocks("test2_t"), &[Block::T]);
        assert_eq!(split_blocks("deviter2_c"), &[Block::C]);
        assert_eq!(split_blocks("test2_mock"), &[Block::Mock]);
        assert_eq!(split_blocks("span_test2"), &[Block::T, Block::C]);
    }

    #[test]
    fn r2_eval_split_classes_cover_the_round2_population_ids() {
        // Amendment 10.8's closed list, read from grammar-a-form's source.
        let src = include_str!("../../grammar-a-form/src/profile.rs");
        let at = src.find("pub const R2_POPULATIONS").unwrap();
        let body = &src[at..at + src[at..].find("];").unwrap()];
        let body = &body[body.find('=').unwrap()..];
        let ids: BTreeSet<&str> = body.split('"').skip(1).step_by(2).collect();
        assert_eq!(ids.len(), 14);
        let mut ours: BTreeSet<&str> = OPEN_SPLITS.iter().chain(&SEALED_SPLITS).copied().collect();
        ours.extend(REFUSED_SPLITS.iter().filter(|s| s.contains('2')));
        assert_eq!(ours, ids);
        // Every id has a block, except Train-2.
        for s in ids.iter().filter(|s| **s != "train2") {
            assert!(!split_blocks(s).is_empty(), "{s}");
        }
    }

    #[test]
    fn r2_eval_sealed_splits_are_named_and_compared_with_the_bound_binary() {
        let fx = fixture();
        let mut cfg = fx.cfg.clone();
        for (i, file, form) in [(0usize, "tn", "N"), (1, "tr", "R")] {
            cfg["populations"][i]["split"] = json!("test2_t");
            let rows: Vec<Value> = t_rows(form)
                .into_iter()
                .map(|mut r| {
                    r["split"] = json!("test2_t");
                    r
                })
                .collect();
            write_lines(&fx.dir.path().join(format!("{file}.jsonl")), &rows);
        }
        assert!(err(run_value(&fx, &cfg, "sealed")).contains("consume_sealed_splits"));
        cfg["consume_sealed_splits"] = json!(["test2_t"]);
        let e = err(run_value(&fx, &cfg, "sealed2"));
        assert!(e.contains("release_evaluations"), "{e}");
        assert!(!fx.dir.path().join("sealed2-margins.json").exists());
        let (pn, _) = release_eval(&fx, "T.N", &[]);
        let (pr, _) = release_eval(&fx, "T.R", &[]);
        cfg["release_evaluations"] = json!({"T.N": pn});
        assert!(err(run_value(&fx, &cfg, "sealed3")).contains("population T.R"));
        cfg["release_evaluations"] = json!({"T.N": pn, "T.R": pr});
        let out = run_value(&fx, &cfg, "sealed4").unwrap();
        assert_eq!(out["consumed_sealed_splits"], json!(["test2_t"]));
        assert_eq!(out["per_seed_equality"]["pass"], true);
        assert_eq!(
            out["per_seed_equality"]["release_evaluations"]
                .as_object()
                .unwrap()
                .len(),
            2
        );
        // Naming a sealed split that no population uses is refused.
        let mut cfg2 = cfg.clone();
        cfg2["consume_sealed_splits"] = json!(["test2_t", "test2_c"]);
        assert!(err(run_value(&fx, &cfg2, "sealed5")).contains("consume_sealed_splits"));
        // A split whose block is not the population's is refused before any read.
        let mut cfg3 = fx.cfg.clone();
        cfg3["populations"][2]["split"] = json!("deviter2_t");
        assert!(err(run_value(&fx, &cfg3, "blk")).contains("does not hold block C"));
        assert!(!fx.dir.path().join("blk-margins.json").exists());
    }

    #[test]
    fn r2_eval_rows_files_hold_one_split_read_from_bytes() {
        // A line of any other split refuses the file from its split bytes, before any
        // parse: a malformed old Dev-select row is not decoded (no origin error, no id).
        let fx = fixture();
        let p = fx.dir.path().join("tn.jsonl");
        let mut rows = t_rows("N");
        rows.push(row("ds-1", "dev_select", "bogus_origin", "dsg1", "wikipedia", "primary"));
        write_lines(&p, &rows);
        let e = err(run_value(&fx, &fx.cfg, "ds"));
        assert!(e.contains("names a refused split (dev_select)"), "{e}");
        assert!(e.contains(&format!("line {}", rows.len())), "{e}");
        assert!(!e.contains("ds-1") && !e.contains("bogus"), "{e}");
        assert!(!fx.dir.path().join("ds-margins.json").exists());
        // An unparseable line of another split: refused the same way, not as a JSON error.
        let mut lines: Vec<String> = t_rows("N").iter().map(|r| r.to_string()).collect();
        lines.insert(3, r#"{"split": "test2_t", "text": "unterminated"#.to_string());
        std::fs::write(&p, lines.join("\n") + "\n").unwrap();
        let e = err(run_value(&fx, &fx.cfg, "t2"));
        assert!(e.contains("line 4 names a sealed or single-use split (test2_t)"), "{e}");
        // Rows of another open split, a nested split key that differs, a line without a split key,
        // and an escaped split value.
        for (k, bad) in [
            r#"{"id": "x", "split": "deviter2_c", "origin": "human_only"}"#,
            r#"{"id": "x", "split": "deviter2_t", "meta": {"split": "cal"}}"#,
            r#"{"id": "x", "origin": "human_only"}"#,
            r#"{"id": "x", "split": "deviter2\u005ft"}"#,
            r#"{"id": "x", "split": 7}"#,
        ]
        .iter()
        .enumerate()
        {
            let mut lines: Vec<String> = t_rows("N").iter().map(|r| r.to_string()).collect();
            lines.push(bad.to_string());
            std::fs::write(&p, lines.join("\n") + "\n").unwrap();
            let e = err(run_value(&fx, &fx.cfg, &format!("bad{k}")));
            assert!(e.contains("before any parse"), "{bad}: {e}");
        }
        // The byte reading itself.
        assert_eq!(
            line_splits(br#"{"a": "split", "split" :  "x", "b": {"split":"y"}, "c": "\"split\": \"z\""}"#),
            vec![Some(&b"x"[..]), Some(&b"y"[..])]
        );
        assert_eq!(line_splits(br#"{"source_split": "x"}"#), Vec::<Option<&[u8]>>::new());
        assert_eq!(line_splits(br#"{"split": "a\"b"}"#), vec![None]);
        assert_eq!(check_rows_split(b"\n{\"split\": \"s\"}\n  \n{\"split\":\"s\"}", "s").unwrap(), 2);
    }

    /// SHA-1, for git blob ids (test only).
    fn sha1(data: &[u8]) -> [u8; 20] {
        let mut h: [u32; 5] = [0x67452301, 0xEFCDAB89, 0x98BADCFE, 0x10325476, 0xC3D2E1F0];
        let mut m = data.to_vec();
        m.push(0x80);
        while m.len() % 64 != 56 {
            m.push(0);
        }
        m.extend_from_slice(&((data.len() as u64) * 8).to_be_bytes());
        for chunk in m.chunks(64) {
            let mut w = [0u32; 80];
            for i in 0..16 {
                w[i] = u32::from_be_bytes(chunk[4 * i..4 * i + 4].try_into().unwrap());
            }
            for i in 16..80 {
                w[i] = (w[i - 3] ^ w[i - 8] ^ w[i - 14] ^ w[i - 16]).rotate_left(1);
            }
            let (mut a, mut b, mut c, mut d, mut e) = (h[0], h[1], h[2], h[3], h[4]);
            for (i, wi) in w.iter().enumerate() {
                let (f, k) = match i {
                    0..=19 => ((b & c) | (!b & d), 0x5A827999),
                    20..=39 => (b ^ c ^ d, 0x6ED9EBA1),
                    40..=59 => ((b & c) | (b & d) | (c & d), 0x8F1BBCDC),
                    _ => (b ^ c ^ d, 0xCA62C1D6u32),
                };
                let t = a
                    .rotate_left(5)
                    .wrapping_add(f)
                    .wrapping_add(e)
                    .wrapping_add(k)
                    .wrapping_add(*wi);
                e = d;
                d = c;
                c = b.rotate_left(30);
                b = a;
                a = t;
            }
            for (x, y) in h.iter_mut().zip([a, b, c, d, e]) {
                *x = x.wrapping_add(y);
            }
        }
        let mut out = [0u8; 20];
        for (i, x) in h.iter().enumerate() {
            out[4 * i..4 * i + 4].copy_from_slice(&x.to_be_bytes());
        }
        out
    }

    fn git_blob_id(b: &[u8]) -> String {
        let mut v = format!("blob {}\0", b.len()).into_bytes();
        v.extend_from_slice(b);
        hex::encode(sha1(&v))
    }

    #[test]
    fn r2_eval_compiled_bound_evaluator_is_the_bound_files() {
        assert_eq!(hex::encode(sha1(b"abc")), "a9993e364706816aba3e25717850c26c9cd0d89d");
        assert_eq!(git_blob_id(b""), "e69de29bb2d1d6434b8b29ae775ad8c2e48c5391");
        // BUILD-PLAN flag G4: the in-process comparison runs the bound library.
        assert_eq!(git_blob_id(include_bytes!("comp_eval.rs")), BOUND_COMP_EVAL_BLOB);
        assert_eq!(
            git_blob_id(include_bytes!("../examples/a_comp_evaluate.rs")),
            BOUND_A_COMP_EVALUATE_BLOB
        );
        assert!(BOUND_EVALUATOR.contains(BOUND_COMP_EVAL_BLOB));
        assert!(BOUND_EVALUATOR.contains(BOUND_A_COMP_EVALUATE_BLOB));
    }

    #[test]
    fn r2_eval_normal_quantile_and_slice_k() {
        for (p, z) in [
            (0.975, 1.9599639845400536),
            (0.995, 2.5758293035489),
            (0.9, 1.2815515655446008),
            (1.0 - 0.05 / 15.0, 2.7130518884727204),
            (0.95, 1.6448536269514715),
        ] {
            assert!((normal_quantile(p) - z).abs() < 1e-12, "p {p}");
            assert!((normal_quantile(1.0 - p) + z).abs() < 1e-12, "p {p}");
        }
        assert_eq!(normal_cdf(0.0), 0.5);
        // §11.4 (V): K = 15 gives 2.713.
        assert_eq!((z_k(15) * 1000.0).round() / 1000.0, 2.713);
        // A fixed point at K = 3.
        let (k, z, w) = slice_gate_k(&[0.001, 0.001, 0.0115, 0.02]);
        assert_eq!((k, w), (3, vec![true, true, true, false]));
        assert_eq!(z, Some(z_k(3)));
        // No fixed point (item 15.3): two slices worthy at z_1 but not at z_2; K = 2 and none is gated.
        let s = 0.03 / 1.8;
        assert_eq!(slice_gate_k(&[s, s]), (2, Some(z_k(2)), vec![false, false]));
        assert_eq!(slice_gate_k(&[]), (0, None, vec![]));
        let (k, _, w) = slice_gate_k(&[0.001; 15]);
        assert_eq!((k, w.iter().filter(|x| **x).count()), (15, 15));
    }

    #[test]
    fn r2_eval_blends() {
        let fx = fixture();
        let out = run_value(&fx, &fx.cfg, "blend").unwrap();
        for (bid, p, m) in [
            ("Cand", "Cand.p", "Cand.m"),
            ("Recal", "Recal.p", "Recal.m"),
        ] {
            let b = &out["blends"][bid];
            let mut vals = vec![];
            for s in RECIPE_SEEDS {
                let k = s.to_string();
                let tn = f(&out["systems"]["T.N"][p][&k]["gate_weight_brier"]);
                let tr = f(&out["systems"]["T.R"][p][&k]["gate_weight_brier"]);
                let cn = f(&out["systems"]["C.N"][m][&k]["gate_weight_brier"]);
                let v = 0.5 * (0.5 * tn + 0.5 * tr) + 0.5 * cn;
                assert_eq!(f(&b["seeds"][&k]["blend"]).to_bits(), v.to_bits());
                vals.push(v);
            }
            assert_eq!(
                f(&b["seed_mean"]).to_bits(),
                (vals.iter().sum::<f64>() / 3.0).to_bits()
            );
        }
        // The tie interval's point is the difference of the three-seed mean blends.
        let d = f(&out["blends"]["Cand"]["seed_mean"]) - f(&out["blends"]["Recal"]["seed_mean"]);
        assert!((f(&gate(&out, "tie")["levels"]["recipe"]["point"]) - d).abs() < 1e-12);
        assert_eq!(
            gate(&out, "tie")["levels"]["recipe"]["units"],
            json!({"T": 24, "C": 18})
        );
    }

    #[test]
    fn r2_eval_r_form_humans_unchanged() {
        let fx = fixture();
        // Drop a human from the R-form rows.
        let tr = fx.dir.path().join("tr.jsonl");
        let kept: Vec<Value> = t_rows("R")
            .into_iter()
            .filter(|r| r["id"] != "th-5-0")
            .collect();
        write_lines(&tr, &kept);
        assert!(err(run_value(&fx, &fx.cfg, "forms")).contains("R-form humans"));
        // Same humans; a system scores one of them differently in the two populations:
        // first the loss and flag, then the flag alone (p_human moved within the
        // probability tolerance, p_model_only and p_mixed unchanged).
        write_lines(&tr, &t_rows("R"));
        let p = fx.dir.path().join("pred-tr-R0.p-17.jsonl");
        let orig = std::fs::read_to_string(&p).unwrap();
        let edit = |g: &dyn Fn(&mut Value)| {
            let mut lines: Vec<Value> = orig.lines().map(|l| serde_json::from_str(l).unwrap()).collect();
            g(lines.iter_mut().find(|l| l["id"] == "th-5-0").unwrap());
            write_lines(&p, &lines);
        };
        edit(&|h| {
            h["p_human"] = json!(0.5);
            h["p_model_only"] = json!(0.3);
            h["p_mixed"] = json!(0.2);
            h["share"] = json!(0.3);
        });
        assert!(err(run_value(&fx, &fx.cfg, "forms2")).contains("scores differently"));
        edit(&|h| h["p_human"] = json!(f(&h["p_human"]) - 5e-7));
        assert!(err(run_value(&fx, &fx.cfg, "forms3")).contains("scores differently"));
        std::fs::write(&p, &orig).unwrap();
        run_value(&fx, &fx.cfg, "forms4").unwrap();
    }

    #[test]
    fn r2_eval_config_refusals() {
        let fx = fixture();
        let mut cfg = fx.cfg.clone();
        cfg["schema"] = json!("other");
        assert!(parse_config(&serde_json::to_vec(&cfg).unwrap()).is_err());
        let mut cfg = fx.cfg.clone();
        cfg["populations"][2]["weights"] = json!({"rule": "balanced"});
        assert!(err(run_value(&fx, &cfg, "w")).contains("block C cohort weights"));
        // A one-seed candidate at the recipe level (R0.u, no longer listed as a comparator).
        let mut cfg = fx.cfg.clone();
        cfg["comparators"] = json!(["R0.p", "Recal.p", "Faux.u", "Recal.m", "R0.t"]);
        cfg["gates"][0]["levels"] = json!(["recipe"]);
        cfg["gates"][0]["candidate"] = json!("R0.u");
        assert!(err(run_value(&fx, &cfg, "r")).contains("recipe level needs seeds"));
        let mut cfg = fx.cfg.clone();
        cfg["gates"][0]["unknown"] = json!(1);
        assert!(parse_config(&serde_json::to_vec(&cfg).unwrap()).is_err());
        // A C-block row in a T population.
        let mut rows = t_rows("N");
        rows[0]["block"] = json!("C");
        write_lines(&fx.dir.path().join("tn.jsonl"), &rows);
        assert!(err(run_value(&fx, &fx.cfg, "b")).contains("is in block C"));
    }
}
