//! B10's second part (BUILD-PLAN section 4, waves 4–5): the gates, measures
//! and records that part 1's `PENDING` list named, on part 1's populations,
//! weights, bootstrap and margins. Numeric only, like part 1: rows are read
//! through [`comp_eval::parse_rows`] and the named non-text fields below, by
//! key; no text field is read, printed or inspected.
//!
//! **Gates** (all evaluated only when the per-seed values equal the bound
//! evaluator's, and after the margins file is written):
//! - `slices` (T2, `r2_slice_gate_v2`): the S4-1 plan record ([`slice_plan`],
//!   from pipeline counts and S3-4's SD, before any prediction is read) fixes
//!   the gated set and z_K; each gated slice passes iff point / SE_realised ≤
//!   z_K, SE_realised the SD (n − 1) of the slice's 1,000 bootstrap draws;
//! - `fs` (T5b, T5e): FS(candidate) − FS(comparator), FS the balanced mean
//!   over paired positives of |p_R(d) − p_N(d)| (uncalibrated involvement; a
//!   pair with a failed or missing prediction takes the largest difference,
//!   1), pairs from B3's `twin_of` and `stratum` fields, both blocks pooled
//!   for T5e; `fri` reports FRI;
//! - `shift` (T3f, the H-inj IV): the paired shift of human-only Brier,
//!   H-inj twin − original, on B14's twins (`probe_of`, the
//!   `slop_ninja_a_form_inject_probe_v1` manifest), one-sided for H-inj-C and
//!   two-sided for H-inj-L, with the line-form L twins reported as a stratum.
//!   T3g is part 1's `cp_flags` on the twin population;
//! - `slope` (T3e): the slope of the uncalibrated involvement logit on the
//!   within-domain standardised MWL field among humans, balanced weights;
//! - `era_slope` (T3h): |β_cand| − |β_comp|, β the within-domain crawl-year
//!   slope of calibrated involvement on humans (`within_slope`, the method of
//!   `slop_ninja_cc_human_measure`, copied and pinned by its source text);
//! - `slack` (T4a, T4b): B11's cross-fitted slack record against the frozen
//!   bound (δ_m, δ_C from B12's σ record, σ_m), or reported without one;
//! - `t8b` (T8b): T8a's ratio r on the whole mock and by row field.
//!
//! **Measures**: S3-1's s (§12.3, with [`t8a_branch`]) and S3-4's slice
//! planning SD. **Records**: the §9.1 step 4 null check (item 1.10's subset
//! per gate and block, faults (a)–(c), B13's joint pass probability), the
//! §9.2 steps 5–8 selection (eligibility, U7, the tie rule,
//! `selection-record-r2.json`, written last), the §11.10 decision record (the
//! levels each gate requires, external records read by digest), named report
//! sets, and the S6-1 replacement inputs (σ′ and σ_eff from B12's
//! `sigma.json`, the d check, the pure-human FPR at 0.5).
//!
//! Structural checks of every record's configuration run in `validate`,
//! before any population is read.
//!
//! Points the protocol leaves open are listed in [`READINGS`] and recorded in
//! every evaluation; each needs a forward amendment before the step that uses
//! it (§2.1), as amendment 15 did for part 1.

use super::*;
use crate::comp_eval::{
    FRONTIER_POPULATION, SLICE_MIN_FRONTIER_POSITIVES, SLICE_MIN_GROUPS, SLICE_MIN_HUMANS,
    build_slices,
};

pub const SLICE_PLAN_CONFIG_SCHEMA: &str = "slop_ninja_a_r2_slice_plan_config_v1";
pub const SLICE_PLAN_SCHEMA: &str = "slop_ninja_a_r2_slice_plan_v1";
pub const SELECTION_SCHEMA: &str = "slop_ninja_a_r2_selection_v1";
/// B11's slack record (B11 writes it; this is the interface B10 reads).
pub const SLACK_SCHEMA: &str = "slop_ninja_a_r2_slack_v1";
/// B13's planned values for the null check (B13 writes it; interface).
pub const PLANNED_SCHEMA: &str = "slop_ninja_a_r2_planned_v1";
/// B12's σ record (`slop_ninja_a_gates sigma`).
pub const SIGMA_SCHEMA: &str = "slop_ninja_a_sigma_v1";
/// B14's text-free twin manifest line.
pub const PROBE_SCHEMA: &str = "slop_ninja_a_form_inject_probe_v1";
pub const PROBE_KINDS: [&str; 2] = ["h_inj_c", "h_inj_l"];
/// B3's raw twin stratum, id prefix and screen-fail stratum (grammar-a-positives finalize.rs).
pub const RAW_TWIN_STRATUM: &str = "raw_twin";
pub const RAW_TWIN_ID_PREFIX: &str = "rform:";
pub const SCREEN_FAIL_STRATUM: &str = "rform_screen_fail_v1";
/// Item 1.10: the null check's noninferiority and no-worse subset, and the
/// gates computed for Faux 29 and reported only.
pub const NULL_SUBSET: [&str; 8] = ["T1a", "T1c", "T1e", "T3a", "T3b", "T3c", "T3e", "T5d"];
pub const NULL_REPORTED: [&str; 5] = ["T1b", "T5b", "T5e", "T3f", "T3g"];
/// Item 1.10 fault (b): planned pass probability at Δ = 0 at or above this.
pub const NULL_PLANNED_PASS: f64 = 0.95;
/// Item 1.10 fault (c): a null-check point outside ± this many planned SEs.
pub const NULL_SE_MULTIPLE: f64 = 2.576;
/// §11.10's gate list for a retrained candidate.
pub const DECISION_GATES: [&str; 27] = [
    "T1a", "T1b", "T1c", "T1d", "T1e", "T8a", "T2", "T3a", "T3b", "T3c", "T3e", "T3f", "T3g", "T3h",
    "T4a", "T4b", "T5a", "T5b", "T5d", "T5e", "T6a", "T6b", "T6c", "T7a", "T7b", "T7c", "T7e",
];
/// §11.10 with item 1.9: reported, not required, for a calibration-only candidate.
pub const CALIBRATION_ONLY_REPORTED: [&str; 5] = ["T1c", "T3f", "T3g", "T5b", "T5e"];
/// §12.3: s is per 1,000 self-weighted documents.
pub const S_DOCS: f64 = 1000.0;
/// Item 1.13: d = 0.025 on 2,000 batches of 1,024 documents, in parts per billion.
pub const D_CHECK_D_PPB: u32 = 25_000_000;
pub const D_CHECK_BATCHES: usize = 2000;
pub const D_CHECK_BATCH_DOCS: usize = 1024;
/// The round-2 recipe's seed pairs (item 1.13).
pub const SEED_PAIRS: [(u64, u64); 3] = [(17, 29), (17, 41), (29, 41)];
/// T3e's logit at p = 0 and p = 1 takes the nearest f64 inside (0, 1)
/// (reading R3): the smallest positive f64, 2^-1074, and the largest f64
/// below 1, 1 − 2^-53. Derived from the format; no value is tuned.
pub const LOGIT_LOW: f64 = f64::from_bits(1);
pub const LOGIT_HIGH: f64 = 1.0 - f64::EPSILON / 2.0;
/// The manifest's σ_m on its 1e-18 grid (V4: 0.035 at n = 1,000).
pub const SIGMA_M_SCALED: u64 = 35_000_000_000_000_000;
/// The manifest's score grid (1e-18) and δ's 1% floor on it (item 1.2).
pub const SCORE_GRID: u128 = 1_000_000_000_000_000_000;
pub const DELTA_FLOOR_SCALED: u128 = 10_000_000_000_000_000;
/// B12's design for σ′ (item 1.13) and for σ_C (S1-4, item 12.4), and its mix
/// in B12's origin order (human_only, mixed, model_only).
pub const SIGMA_N_PRIME: u64 = 1000;
pub const SIGMA_N_C: u64 = 4000;
pub const SIGMA_MIX: [f64; 3] = [0.20, 0.55, 0.25];
/// §11.4's gated CC strata (a C row's stratum is its domain, item 15.1).
pub const C_SLICE_STRATA: [&str; 2] = ["essays", "news"];
/// §11.4's pooled slices (T and C), each block at half weight (reading R1).
pub const POOLED_SLICES: [&str; 2] = ["held_out_generator", "frontier"];
pub const POOLED_COEF: f64 = 0.5;
/// The external gate record the decision record reads (T5a, T6a–T6c, T7a–T7c, T7e).
pub const EXTERNAL_SCHEMA: &str = "slop_ninja_a_r2_external_gate_v1";
/// §11.10's gates that B10 does not evaluate: their records come from other tools.
pub const EXTERNAL_GATES: [&str; 8] = ["T5a", "T6a", "T6b", "T6c", "T7a", "T7b", "T7c", "T7e"];
/// The manifest's operating threshold (item 1.13).
pub const FLOOR_THRESHOLD: f64 = 0.5;

pub const SLICE_PLAN_RULE: &str = "PROTOCOL-A2 §11.4 and §12.6 S4-1, with amendment 15 item 15.3: the candidate family is derived from the rows (every Test-2 T domain; the CC strata essays and news, a C row's stratum being its domain; held_out_generator and frontier, T and C pooled at half weight each); a slice is a candidate iff it meets the 15-group / 150-human minimum (and 150 frontier positives for frontier) from pipeline counts; SE_planned = S3-4's SD (read from the B10 evaluation record that measured it, by digest) / sqrt(the slice's realised source groups); K and the gated set by r2_slice_gate_v2; written before any prediction is read";
pub const T2_RULE: &str = "PROTOCOL-A2 §11.4 T2 (r2_slice_gate_v2): each gated slice passes iff (candidate - comparator) point / SE_realised <= z_K; SE_realised = the SD (n-1) of the slice's 1,000 protocol bootstrap draws; slice weights = the population's gate-weight rule recomputed over the slice's documents; a pooled slice is 1/2 its T term + 1/2 its C term (reading R1), blocks resampled independently (item 15.2); the plan is re-derived from the rows and S3-4's record and must equal the one hashed";
pub const FS_RULE: &str = "PROTOCOL-A2 §11.7 with §11's worst case: FS = the balanced mean over paired positives of |p_R(d) - p_N(d)|, p the uncalibrated involvement min(p_model_only + p_mixed, 1); a pair whose N-form or R-form prediction failed or is missing at any seed of the level takes the largest possible difference, 1, for the candidate and the comparator alike (reading R6); pairs are B3's raw twins (stratum raw_twin, twin_of the N-form id, id rform:<id>) of the N-form population's primary positives; balanced weights recomputed over the paired N-form positives (over the union of both blocks' paired positives when pooled, T5e, reading R15); differences paired by source group, blocks resampled independently";
pub const SHIFT_RULE: &str = "PROTOCOL-A2 §11.5 T3f (IV): the paired shift of human-only Brier, H-inj twin - original, for one system; each twin weighs its original's human-only balanced weight (S2 H1) and resamples with its original's source group; H-inj-C passes iff draws[974] <= m; H-inj-L (two-sided) iff draws[24] >= -m and draws[974] <= m; recipe m, artifact sqrt(2) m";
pub const SLOPE_RULE: &str = "PROTOCOL-A2 §11.5 T3e: among the population's primary humans, the weighted least-squares slope (human-only balanced weights) of the uncalibrated involvement logit (exact for 0 < p < 1; p = 0 and p = 1 at the nearest f64 inside (0, 1), reading R3) on the named MWL field standardised within domain (weighted mean and SD over the domain's humans); recipe: per-document logits averaged over the seeds; (candidate - comparator) upper <= m (reseed SD of the slope over the comparator recipe); bootstrap over source groups recomputing both slopes";
pub const ERA_RULE: &str = "PROTOCOL-A2 §11.5 T3h as amended by item 1.8: beta = the within-domain crawl-year slope of calibrated involvement on the population's primary humans (slop_ninja_cc_human_measure's within_slope, unweighted); (|beta_cand| - |beta_comp|) upper <= sqrt(2) m_beta at artifact level, m_beta the reseed SD of |beta| over the comparator recipe; bootstrap over source groups recomputing both slopes; the logit-scale variant (uncalibrated involvement logit, reading R3) is reported only";
pub const SELECTION_RULE: &str = "PROTOCOL-A2 §9.2 steps 5-9 (U7, R6): a candidate is eligible iff every required condition's gate passes (human-only per block, the T-block R-form condition; retrained candidates also the H-inj shifts and FS; R0-recal exempt from H-inj and FS, reported); among the eligible the lowest three-seed mean blended_brier_r2_v2 wins; tie rule: top two only, if their paired 95% interval (recipe, 1/4 T N + 1/4 T R + 1/2 C N, blocks independent) includes zero the earlier in the order wins; none eligible: R0 stays; the record holds every candidate's Dev-select-2 values (its blend and each condition gate's levels)";

/// Points the protocol leaves open that part 2's code settles. Each needs a
/// forward amendment before the step that first uses it (§2.1; the
/// amendment-15 precedent); recorded in every evaluation.
pub const READINGS: [&str; 16] = [
    "R1 T2 slice weights: the population's gate-weight rule recomputed over the slice's documents (balanced in T, cohort in C); a pooled T+C slice (held_out_generator, frontier) is 1/2 its T term + 1/2 its C term (blended_brier_r2_v2's block weights, V15), blocks resampled independently (item 15.2)",
    "R2 T2 SE_realised: the SD (n-1) of the slice statistic's 1,000 bootstrap draws; a slice with SE 0 or not finite is not evaluated",
    "R3 T3e: z = within-domain standardisation with human-only balanced weights (weighted mean, weighted SD with ddof 0; a domain with SD 0 gives z = 0); the exact logit of involvement for 0 < p < 1, and for p = 0 and p = 1 the logit of the nearest f64 inside (0, 1), 2^-1074 and 1 - 2^-53, which keeps the order of involvements and tunes no value; a failed or missing prediction enters at the bound rule's worst case 1 - y; recipe averages per-document logits over seeds; standardisation fixed on the full sample, not per bootstrap draw",
    "R4 S3-4: per-group paired difference = the group's balanced-weight mean difference (G times the group's weighted sum), candidate seed 17 - R0 at the product export, N-form; pooled within-domain SD = sqrt(sum over domains of squared deviations from the domain mean / sum (groups - 1)); a source group spanning domains is refused",
    "R5 S3-1: the paired SE of r is the SD (n-1) of T8a's 1,000 ratio draws over ledger units at artifact level (candidate seed 17, R0 shipped); ESS is the Kish ESS of the mock's W_A8 x 20/55/25 row weights; s = SE x sqrt(ESS/1,000)",
    "R6 FS: per system and level, a pair whose N-form or R-form prediction failed or is missing at any seed of the level takes the largest possible difference, |p_R - p_N| = 1 (§11's worst case), for the candidate and the comparator alike, and failed pairs are counted per pair set; balanced weights are recomputed over the paired positives; the screen-fail stratum is never paired",
    "R7 T3f: twins carry their original's human-only balanced weight and source group; the line-form stratum (form_inject.layout.line_form) is reported with balanced weights recomputed over its originals",
    "R8 T3h: calibrated involvement, crawl year from a named integer row field, unweighted within-domain OLS; |beta| difference bootstrapped by source group; the reported uncalibrated logit-scale slope takes R3's logit",
    "R9 T8b: a by-field subset is the rows with that value, plus every human when the field is absent on humans (comp_eval's positives_with_all_humans); r and its bootstrap as T8a",
    "R10 selection tie interval: the paired composite of the two blends' terms at recipe level with coefficients 1/4, 1/4, 1/2, one stream over blocks T then C (item 15.2)",
    "R11 null check: item 1.10's subset is recorded per gate and block; fault (c) compares the gate's artifact-level point estimate with B13's planned SE for that gate and block (key <gate>.<block>); gates without a point (T3a, T3b) have no fault (c); a subset gate not evaluated is counted apart from the pass rate, which is recorded beside B13's joint pass probability at zero",
    "R12 d check: involvement min(p_model_only + p_mixed, 1) in parts per billion by the manifest's Probability::from_f64 (p x 10^9 rounded half to even; a non-finite or out-of-range value refused), a failed or missing prediction left out (near_stats); d stays at 0.025 only if no seed pair falls within 0.025 on any one of the 2,000 batches; each batch is 1,024 distinct documents of the consumed mock, read from a file whose draw rule is not yet fixed",
    "R13 pure-human FPR at 0.5: flag = 1 - p_human >= 0.5 (the bound evaluator's human_fpr_at_0_5); counts reported, the manifest's floors and their comparison rule not yet applied",
    "R14 T4a/T4b: B11's record is cross-fitted on the gating population's rows and names its export and the CAL population that export was fitted on (product on cal2_t, mainnet on cal2_c, testnet on cal2_mock); it states the slack as a share of the system's own B0 per seed, and each seed of the level must be <= the bound: T4a T delta_m = 0.070 and T4b sigma_m = 0.035 (V4), T4a C delta_C = B12's delta at n = 4,000 under 20/55/25 on Dev-iter-2 C with cohort_weight (S1-4, item 12.4), bound by that sigma record's digest; a record without a bound is reported",
    "R15 T5e: held-out-family positives are the paired positives whose generator family (generator_family, or the bound default family) is in the gate's list; both blocks pooled means FS is the balanced mean over the union of both blocks' paired held-out positives, with balanced weights recomputed over that union (source groups keyed by block) and each block resampled independently",
    "R16 decision record: a protocol gate mapped to several configuration gates (per block) passes iff each passes, each evaluated at exactly the levels §11.10 requires (T3e recipe only, §11.5; T3h artifact only, §11.5 and item 1.8; every other B10 gate artifact and recipe); T5a, T6a-T6c, T7a-T7c and T7e come from external records (slop_ninja_a_r2_external_gate_v1) whose own status is read; a gate not evaluated is recorded as not evaluated and never passes; a reported gate cannot be required",
];

// ------------------------------------------------------------------ shared helpers

/// Involvement of one document by the bound scoring rule; worst case (1 − y)
/// for a failed or missing prediction.
pub fn involvement_doc(r: &Row, o: Option<&Obs>) -> f64 {
    match o {
        Some(Obs::Ok(p)) => (p.p_model_only + p.p_mixed).min(1.0),
        _ => 1.0 - r.y,
    }
}

/// Per-primary-position values for a level (artifact: the released or only
/// seed; recipe: the seed mean), with part 1's seed rules.
pub(super) fn level_values(
    seeds: &BTreeMap<u64, SeedRun>,
    level: Level,
    released: u64,
    recipe_side: bool,
    get: &dyn Fn(&SeedRun, usize) -> f64,
) -> Result<(Vec<f64>, Vec<u64>)> {
    match level {
        Level::Artifact => {
            let r = pick(seeds, released)?;
            Ok(((0..r.loss.len()).map(|p| get(r, p)).collect(), vec![r.seed]))
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
                .map(|p| rs.iter().map(|r| get(r, p)).sum::<f64>() / rs.len() as f64)
                .collect();
            Ok((v, s))
        }
    }
}

fn loss_of(r: &SeedRun, p: usize) -> f64 {
    r.loss[p]
}

fn inv_of(r: &SeedRun, p: usize) -> f64 {
    r.inv[p]
}

/// T3e's involvement logit (reading R3): exact for 0 < p < 1; p = 0 and
/// p = 1 take the nearest f64 inside (0, 1), so the order is kept.
pub fn logit(p: f64) -> f64 {
    let c = if p <= 0.0 {
        LOGIT_LOW
    } else if p >= 1.0 {
        LOGIT_HIGH
    } else {
        p
    };
    (c / (1.0 - c)).ln()
}

/// Per-primary-position failure for a level: the released (or only) seed's,
/// or any seed's at the recipe level (call after [`level_values`]).
pub(super) fn level_failed(seeds: &BTreeMap<u64, SeedRun>, level: Level, released: u64) -> Result<Vec<bool>> {
    match level {
        Level::Artifact => Ok(pick(seeds, released)?.fail.clone()),
        Level::Recipe => {
            let rs: Vec<&SeedRun> = seeds.values().collect();
            let n = rs.first().map_or(0, |r| r.fail.len());
            Ok((0..n).map(|p| rs.iter().any(|r| r.fail[p])).collect())
        }
    }
}

/// The bound balanced weights over the union of several populations'
/// documents, source groups keyed by block (reading R15); with one
/// population these are `comp_eval::balanced_weights` exactly.
pub fn union_balanced_weights(parts: &[(&Population, Vec<usize>)]) -> Vec<Vec<f64>> {
    if let [(p, idx)] = parts {
        return vec![balanced_weights(&p.rows, idx)];
    }
    let mut rows: Vec<Row> = vec![];
    let mut spans = vec![];
    for (p, idx) in parts {
        let start = rows.len();
        for &i in idx {
            let mut r = p.rows[i].clone();
            r.group = format!("{}\u{1f}{}", p.block.name(), r.group);
            rows.push(r);
        }
        spans.push(start..rows.len());
    }
    let all: Vec<usize> = (0..rows.len()).collect();
    let w = balanced_weights(&rows, &all);
    spans.into_iter().map(|r| w[r].to_vec()).collect()
}

fn logit_of(r: &SeedRun, p: usize) -> f64 {
    logit(r.inv[p])
}

/// The comparator for a level: the recipe override at the recipe level.
fn side<'a>(level: Level, comp: &'a str, recipe: Option<&'a String>) -> &'a str {
    if level == Level::Recipe {
        recipe.map(String::as_str).unwrap_or(comp)
    } else {
        comp
    }
}

/// Bounds and bootstrap SE of a composite over blocks: (lower, upper, SE).
pub fn interval(blocks: &[(Block, Vec<f64>)]) -> (Option<f64>, Option<f64>, Option<f64>) {
    let refs: Vec<&[f64]> = blocks.iter().map(|(_, v)| v.as_slice()).collect();
    match stratified_bootstrap(&refs) {
        Some(d) => (Some(d[24]), Some(d[974]), sample_sd(&d)),
        None => (None, None, None),
    }
}

fn units_json(blocks: &[(Block, Vec<f64>)]) -> Value {
    Value::Object(
        blocks
            .iter()
            .map(|(b, v)| (b.name().to_string(), json!(v.len())))
            .collect(),
    )
}

fn pop<'a>(ctx: &'a Ctx, id: &str, gate: &str) -> Result<&'a Population> {
    ctx.pops
        .get(id)
        .with_context(|| format!("gate {gate}: unknown population {id}"))
}

fn opt_str<'a>(p: &'a Population, field: &str, pos: usize) -> Option<&'a str> {
    p.opt.get(field)?.get(pos)?.as_ref()?.as_str()
}

fn opt_f64(p: &Population, field: &str, pos: usize) -> Option<f64> {
    p.opt.get(field)?.get(pos)?.as_ref()?.as_f64()
}

fn status_of(reasons: &[String], all: bool, gated: bool) -> &'static str {
    if !gated {
        "reported"
    } else if !reasons.is_empty() {
        "not_evaluated"
    } else if all {
        "pass"
    } else {
        "fail"
    }
}

/// Weights over a subset of primary positions by the population's gate-weight
/// rule recomputed over those documents (reading R1).
pub(super) fn subset_weights(p: &Population, pos: &[usize]) -> Result<Vec<f64>> {
    let idx: Vec<usize> = pos.iter().map(|&q| p.primary[q]).collect();
    match p.block {
        Block::T => Ok(balanced_weights(&p.rows, &idx)),
        Block::C => Ok(cohort_weights(
            &p.rows,
            &idx,
            p.table.as_ref().context("a C population without its share table")?,
        )?
        .0),
        Block::Mock => bail!(
            "population {}: a mock population has no slice weights",
            p.id
        ),
    }
}

// ------------------------------------------------------------------ probes (B14 twins)

/// A population of B14's H-inj twins of another population's humans.
pub struct Probe {
    pub kind: String,
    pub of: String,
    /// `probe_of` per primary position.
    pub of_ids: Vec<String>,
    /// `form_inject.layout.line_form` per primary position (H-inj-L).
    pub line_form: Vec<Option<bool>>,
    pub manifest: PathBuf,
    pub manifest_sha256: String,
    pub manifest_lines: usize,
}

/// The fields a probe population loads.
pub const PROBE_FIELDS: [&str; 2] = ["probe_of", "form_inject"];

/// Check a twin population on load: every primary row is a human twin with
/// `probe_of`, id `probe:<kind>:<probe_of>`, its `form_inject` group the
/// spec's kind; the manifest (one split, read from line bytes first) names
/// exactly these (probe_id, base_id) pairs, kind, split and origin.
pub(super) fn load_probe(
    spec: &ProbeSpec,
    split: &str,
    rows: &[Row],
    primary: &[usize],
    opt: &BTreeMap<String, Vec<Option<Value>>>,
) -> Result<Probe> {
    ensure!(
        PROBE_KINDS.contains(&spec.kind.as_str()),
        "probe kind {} is not one of {PROBE_KINDS:?}",
        spec.kind
    );
    ensure!(
        primary.len() == rows.len(),
        "a twin population holds primary rows only"
    );
    let mut of_ids = Vec::with_capacity(primary.len());
    let mut line_form = Vec::with_capacity(primary.len());
    let mut pairs = BTreeSet::new();
    for (q, &i) in primary.iter().enumerate() {
        let r = &rows[i];
        ensure!(r.is_human(), "twin {}: not human_only", r.id);
        let of = opt["probe_of"][q]
            .as_ref()
            .and_then(Value::as_str)
            .with_context(|| format!("twin {}: no probe_of", r.id))?;
        ensure!(
            r.id == format!("probe:{}:{of}", spec.kind),
            "twin {}: id is not probe:{}:<probe_of> (B14)",
            r.id,
            spec.kind
        );
        let fi = opt["form_inject"][q]
            .as_ref()
            .with_context(|| format!("twin {}: no form_inject record", r.id))?;
        ensure!(
            fi["group"] == spec.kind.as_str(),
            "twin {}: form_inject group is not {}",
            r.id,
            spec.kind
        );
        line_form.push(fi.pointer("/layout/line_form").and_then(Value::as_bool));
        ensure!(
            pairs.insert((r.id.clone(), of.to_string())),
            "twin {} twice",
            r.id
        );
        of_ids.push(of.to_string());
    }
    let bytes = read(&spec.manifest)?;
    check_rows_split(&bytes, split).with_context(|| {
        format!("twin manifest {}", spec.manifest.display())
    })?;
    let mut seen = BTreeSet::new();
    let mut lines = 0;
    for (k, l) in std::str::from_utf8(&bytes)
        .context("twin manifest is not UTF-8")?
        .lines()
        .enumerate()
    {
        if l.trim().is_empty() {
            continue;
        }
        let m: Value =
            serde_json::from_str(l).with_context(|| format!("twin manifest line {}", k + 1))?;
        ensure!(
            m["schema"] == PROBE_SCHEMA
                && m["kind"] == spec.kind.as_str()
                && m["origin"] == "human_only"
                && m["split"] == split,
            "twin manifest line {}: not a {PROBE_SCHEMA} line of kind {} for split {split}",
            k + 1,
            spec.kind
        );
        let pid = m["probe_id"].as_str().context("probe_id")?.to_string();
        let bid = m["base_id"].as_str().context("base_id")?.to_string();
        ensure!(seen.insert((pid, bid)), "twin manifest line {} repeats a twin", k + 1);
        lines += 1;
    }
    ensure!(
        seen == pairs,
        "the twin manifest's (probe_id, base_id) pairs are not the population's twins ({} lines, {} twins)",
        seen.len(),
        pairs.len()
    );
    Ok(Probe {
        kind: spec.kind.clone(),
        of: spec.of.clone(),
        of_ids,
        line_form,
        manifest: spec.manifest.clone(),
        manifest_sha256: sha256_hex(&bytes),
        manifest_lines: lines,
    })
}

/// A twin population twins exactly its original population's primary humans
/// (same block and split), one twin each.
pub(super) fn check_probes(pops: &BTreeMap<String, Population>) -> Result<()> {
    for p in pops.values() {
        let Some(pr) = &p.probe else { continue };
        let o = pops
            .get(&pr.of)
            .with_context(|| format!("twin population {}: unknown population {}", p.id, pr.of))?;
        ensure!(
            o.probe.is_none() && o.form != Some(Form::R),
            "twin population {}: {} must be an N-form or formless original population",
            p.id,
            o.id
        );
        ensure!(
            o.block == p.block && o.split == p.split,
            "twin population {}: block and split must be {}'s",
            p.id,
            o.id
        );
        let of: BTreeSet<&str> = pr.of_ids.iter().map(String::as_str).collect();
        ensure!(
            of.len() == pr.of_ids.len() && of == o.human_ids(),
            "twin population {}: its twins must be {}'s primary humans, one each",
            p.id,
            o.id
        );
    }
    Ok(())
}

// ------------------------------------------------------------------ T3f (IV)

pub(super) fn shift_gate(ctx: &Ctx, spec: &GateSpec) -> Result<Value> {
    let GateSpec::Shift {
        id,
        population,
        system,
        levels,
        margin,
        two_sided,
    } = spec
    else {
        unreachable!()
    };
    let p = pop(ctx, population, id)?;
    let pr = p
        .probe
        .as_ref()
        .with_context(|| format!("gate {id}: population {population} is not a twin population"))?;
    let o = &ctx.pops[&pr.of];
    let hk: BTreeMap<&str, usize> = o
        .humans
        .iter()
        .enumerate()
        .map(|(k, &q)| (o.rows[o.primary[q]].id.as_str(), k))
        .collect();
    let ks: Vec<usize> = pr.of_ids.iter().map(|x| hk[x.as_str()]).collect();
    let gated = margin.is_some();
    let mut by_level = Map::new();
    let mut reasons = vec![];
    let mut all = true;
    for &level in levels {
        let (lp, sp) = level_values(
            runs(ctx.book, population, system)?,
            level,
            ctx.cfg.released_seed,
            gated,
            &loss_of,
        )
        .with_context(|| format!("gate {id}"))?;
        let (lo, so) = level_values(
            runs(ctx.book, &pr.of, system)?,
            level,
            ctx.cfg.released_seed,
            gated,
            &loss_of,
        )
        .with_context(|| format!("gate {id}"))?;
        ensure!(
            sp == so,
            "gate {id}: {system}'s seeds differ between {population} and {}",
            pr.of
        );
        let term = |js: &[usize], w: Vec<f64>| Term {
            block: p.block,
            units: js.iter().map(|&j| o.units[o.humans[ks[j]]].as_str()).collect(),
            weights: w,
            delta: js.iter().map(|&j| lp[j] - lo[o.humans[ks[j]]]).collect(),
            coef: 1.0,
        };
        let all_js: Vec<usize> = (0..p.primary.len()).collect();
        let w: Vec<f64> = ks.iter().map(|&k| o.human_weights[k]).collect();
        let (point, blocks) = composite_units(&[term(&all_js, w)]);
        let (lower, upper, se) = interval(&blocks);
        let mut rec = json!({"seeds": sp, "twins": p.primary.len(), "units": units_json(&blocks),
            "point": point, "lower": lower, "upper": upper, "bootstrap_se": se});
        if pr.line_form.iter().any(Option::is_some) {
            let mut strata = Map::new();
            for v in [false, true] {
                let js: Vec<usize> = all_js
                    .iter()
                    .copied()
                    .filter(|&j| pr.line_form[j] == Some(v))
                    .collect();
                if js.is_empty() {
                    continue;
                }
                let idx: Vec<usize> = js.iter().map(|&j| o.primary[o.humans[ks[j]]]).collect();
                let (pt, b) = composite_units(&[term(&js, balanced_weights(&o.rows, &idx))]);
                let (lo2, hi2, _) = interval(&b);
                strata.insert(
                    if v { "line_form" } else { "not_line_form" }.into(),
                    json!({"twins": js.len(), "point": pt, "lower": lo2, "upper": hi2, "role": "reported"}),
                );
            }
            rec["strata"] = Value::Object(strata);
        }
        if let Some(m) = margin {
            match margin_value(ctx, m, level)? {
                Err(why) => {
                    reasons.push(why);
                    all = false;
                }
                Ok((mv, src)) => {
                    rec["margin"] = json!(mv);
                    rec["margin_source"] = json!(src);
                    match (lower, upper) {
                        (Some(l), Some(u)) => {
                            let pass = shift_pass(l, u, mv, *two_sided);
                            rec["pass"] = json!(pass);
                            all &= pass;
                        }
                        _ => {
                            reasons.push(format!("{}: fewer than two units", level.name()));
                            all = false;
                        }
                    }
                }
            }
        }
        by_level.insert(level.name().into(), rec);
    }
    Ok(json!({"id": id, "kind": "shift", "rule": SHIFT_RULE, "population": population, "original": pr.of,
        "probe_kind": pr.kind, "system": system, "two_sided": two_sided,
        "criterion": if *two_sided { "draws[24] >= -m and draws[974] <= m" } else { "draws[974] <= m" },
        "difference": "twin_minus_original", "levels": by_level,
        "status": status_of(&reasons, all, gated), "reasons": reasons}))
}

/// T3f's criterion: one-sided `upper ≤ m`, or two-sided `lower ≥ −m` and `upper ≤ m`.
pub fn shift_pass(lower: f64, upper: f64, m: f64, two_sided: bool) -> bool {
    upper <= m && (!two_sided || lower >= -m)
}

// ------------------------------------------------------------------ FS and FRI (T5b, T5e)

/// The paired positives of an N/R pair: (N primary position, R primary position).
pub struct FsPairs {
    pub pairs: Vec<(usize, usize)>,
    pub counts: Value,
}

/// The form-sensitivity pairs of an N-form and an R-form population (see [`FS_RULE`]).
pub fn fs_pairs(n: &Population, r: &Population, families: Option<&[String]>) -> Result<FsPairs> {
    ensure!(
        n.form == Some(Form::N) && r.form == Some(Form::R) && n.block == r.block && n.split == r.split,
        "populations {} and {} are not an N-form and an R-form population of one block and split",
        n.id,
        r.id
    );
    let npos: BTreeMap<&str, usize> = (0..n.primary.len())
        .filter(|&p| !n.rows[n.primary[p]].is_human())
        .map(|p| (n.rows[n.primary[p]].id.as_str(), p))
        .collect();
    let (mut twins, mut unmatched, mut screen_fail, mut filtered) = (0, 0, 0, 0);
    let mut pairs = vec![];
    let mut used = BTreeSet::new();
    for q in 0..r.primary.len() {
        let rr = &r.rows[r.primary[q]];
        if rr.is_human() {
            continue;
        }
        match opt_str(r, "stratum", q) {
            Some(RAW_TWIN_STRATUM) => {}
            Some(SCREEN_FAIL_STRATUM) => {
                screen_fail += 1;
                continue;
            }
            _ => bail!(
                "R-form positive {}: stratum is neither {RAW_TWIN_STRATUM} nor {SCREEN_FAIL_STRATUM}",
                rr.id
            ),
        }
        twins += 1;
        let of = opt_str(r, "twin_of", q)
            .with_context(|| format!("raw twin {}: no twin_of", rr.id))?;
        ensure!(
            rr.id == format!("{RAW_TWIN_ID_PREFIX}{of}"),
            "raw twin {}: id is not {RAW_TWIN_ID_PREFIX}<twin_of> (B3)",
            rr.id
        );
        let Some(&p) = npos.get(of) else {
            unmatched += 1;
            continue;
        };
        let nr = &n.rows[n.primary[p]];
        ensure!(
            nr.group == rr.group,
            "raw twin {} is not in its N-form record's source group",
            rr.id
        );
        ensure!(used.insert(p), "two raw twins of {}", nr.id);
        if let Some(f) = families {
            let fam = nr.family.as_deref().unwrap_or("");
            if !f.iter().any(|x| x == fam) {
                filtered += 1;
                continue;
            }
        }
        pairs.push((p, q));
    }
    pairs.sort();
    let counts = json!({"n_form_positives": npos.len(), "raw_twins": twins, "screen_fail": screen_fail,
        "twins_without_n_form_record": unmatched, "outside_families": filtered, "pairs": pairs.len(),
        "n_form_positives_unpaired": npos.len() - used.len()});
    Ok(FsPairs { pairs, counts })
}

/// One system's per-pair FS values at a level (reading R6): |p_R − p_N|,
/// or 1 when either form's prediction failed or is missing; and the count
/// of such pairs.
fn fs_values(
    ctx: &Ctx,
    id: &str,
    sys: &str,
    (n, r, pp): (&Population, &Population, &[(usize, usize)]),
    level: Level,
    recipe_side: bool,
) -> Result<(Vec<f64>, Vec<u64>, usize)> {
    let get = |popn: &Population| -> Result<(Vec<f64>, Vec<u64>, Vec<bool>)> {
        let seeds = runs(ctx.book, &popn.id, sys)?;
        let (v, s) = level_values(seeds, level, ctx.cfg.released_seed, recipe_side, &inv_of)
            .with_context(|| format!("gate {id}: {sys} on {}", popn.id))?;
        Ok((v, s, level_failed(seeds, level, ctx.cfg.released_seed)?))
    };
    let (vn, sn, fn_) = get(n)?;
    let (vr, sr, fr) = get(r)?;
    ensure!(sn == sr, "gate {id}: {sys}'s seeds differ between {} and {}", n.id, r.id);
    let mut failed = 0;
    let v = pp
        .iter()
        .map(|&(p, q)| {
            if fn_[p] || fr[q] {
                failed += 1;
                1.0
            } else {
                (vr[q] - vn[p]).abs()
            }
        })
        .collect();
    Ok((v, sn, failed))
}

pub(super) fn fs_gate(ctx: &Ctx, spec: &GateSpec) -> Result<Value> {
    let GateSpec::Fs {
        id,
        pairs,
        candidate,
        comparator,
        comparator_recipe,
        levels,
        families,
        margin,
        strict,
    } = spec
    else {
        unreachable!()
    };
    ensure!(!pairs.is_empty() && !levels.is_empty(), "gate {id}: needs pairs and levels");
    let gated = margin.is_some();
    let mut prepared = vec![];
    let mut counts = vec![];
    for t in pairs {
        let n = pop(ctx, &t.n_population, id)?;
        let r = pop(ctx, &t.r_population, id)?;
        let fp = fs_pairs(n, r, families.as_deref()).with_context(|| format!("gate {id}"))?;
        ensure!(!fp.pairs.is_empty(), "gate {id}: no paired positives in {} and {}", n.id, r.id);
        counts.push(json!({"n_population": n.id, "r_population": r.id, "block": n.block.name(), "counts": fp.counts}));
        prepared.push((n, r, fp.pairs));
    }
    // Balanced weights over the paired N-form positives; over their union
    // when both blocks are pooled (T5e, reading R15).
    let parts: Vec<(&Population, Vec<usize>)> = prepared
        .iter()
        .map(|(n, _, pp)| (*n, pp.iter().map(|&(p, _)| n.primary[p]).collect()))
        .collect();
    let ws = union_balanced_weights(&parts);
    let mut by_level = Map::new();
    let mut reasons = vec![];
    let mut all = true;
    for &level in levels {
        let comp = side(level, comparator, comparator_recipe.as_ref());
        let mut ts = vec![];
        let mut fsv = vec![];
        let (mut fs_c, mut fs_b) = (0.0, 0.0);
        for ((n, r, pp), w) in prepared.iter().zip(&ws) {
            let (fc, sa, failed_c) = fs_values(ctx, id, candidate, (n, r, pp), level, true)?;
            let (fb, sb, failed_b) = fs_values(ctx, id, comp, (n, r, pp), level, false)?;
            fs_c += w.iter().zip(&fc).map(|(a, b)| a * b).sum::<f64>();
            fs_b += w.iter().zip(&fb).map(|(a, b)| a * b).sum::<f64>();
            fsv.push(json!({"n_population": n.id, "block": n.block.name(), "pairs": pp.len(),
                "fs_candidate": wmean(w, &fc), "fs_comparator": wmean(w, &fb),
                "failed_pairs_candidate": failed_c, "failed_pairs_comparator": failed_b,
                "seeds_candidate": sa, "seeds_comparator": sb}));
            ts.push(Term {
                block: n.block,
                units: pp.iter().map(|&(p, _)| n.units[p].as_str()).collect(),
                weights: w.clone(),
                delta: fc.iter().zip(&fb).map(|(a, b)| a - b).collect(),
                coef: 1.0,
            });
        }
        let (point, blocks) = composite_units(&ts);
        let (lower, upper, se) = interval(&blocks);
        let mut rec = json!({"comparator": comp, "terms": fsv, "fs_candidate": fs_c, "fs_comparator": fs_b,
            "units": units_json(&blocks), "point": point, "lower": lower, "upper": upper, "bootstrap_se": se});
        if let Some(m) = margin {
            match margin_value(ctx, m, level)? {
                Err(why) => {
                    reasons.push(why);
                    all = false;
                }
                Ok((mv, src)) => {
                    rec["margin"] = json!(mv);
                    rec["margin_source"] = json!(src);
                    rec["comparison"] = json!(if *strict { "upper < margin" } else { "upper <= margin" });
                    match upper {
                        Some(u) => {
                            let pass = margin_pass(u, mv, *strict);
                            rec["pass"] = json!(pass);
                            all &= pass;
                        }
                        None => {
                            reasons.push(format!("{}: fewer than two units in a block", level.name()));
                            all = false;
                        }
                    }
                }
            }
        }
        by_level.insert(level.name().into(), rec);
    }
    Ok(json!({"id": id, "kind": "fs", "rule": FS_RULE, "candidate": candidate, "families": families,
        "pooled": pairs.len() > 1, "difference": "fs_candidate_minus_fs_comparator", "pairs": counts,
        "bootstrap": BOOTSTRAP_METHOD, "levels": by_level, "status": status_of(&reasons, all, gated), "reasons": reasons}))
}

/// FRI: Brier on the N-form population minus Brier on the R-form population
/// (same humans), each at its gate weights, per seed and as the seed mean.
pub(super) fn fri_gate(ctx: &Ctx, spec: &GateSpec) -> Result<Value> {
    let GateSpec::Fri {
        id,
        n_population,
        r_population,
        system,
    } = spec
    else {
        unreachable!()
    };
    let n = pop(ctx, n_population, id)?;
    let r = pop(ctx, r_population, id)?;
    ensure!(
        n.form == Some(Form::N) && r.form == Some(Form::R) && n.block == r.block && n.split == r.split,
        "gate {id}: {n_population} and {r_population} are not an N/R pair"
    );
    let (sn, sr) = (runs(ctx.book, n_population, system)?, runs(ctx.book, r_population, system)?);
    ensure!(seed_list(sn) == seed_list(sr), "gate {id}: seeds differ");
    let mut per = Map::new();
    let mut v = vec![];
    for (s, a) in sn {
        let bn = wmean(&n.weights, &a.loss);
        let br = wmean(&r.weights, &sr[s].loss);
        v.push(bn - br);
        per.insert(s.to_string(), json!({"brier_n_form": bn, "brier_r_form": br, "fri": bn - br}));
    }
    Ok(json!({"id": id, "kind": "fri", "system": system, "n_population": n_population, "r_population": r_population,
        "rule": "FRI = Brier on N-form positives + humans minus Brier on R-form positives + the same humans, each at the population's gate weights (PROTOCOL-A2 §11.7 T5b, reported)",
        "seeds": per, "seed_mean": v.iter().sum::<f64>() / v.len() as f64, "status": "reported", "reasons": []}))
}

// ------------------------------------------------------------------ T3e (MWL slope)

/// Within-domain standardised field values over the population's primary
/// humans (reading R3): (z per human, human weights).
pub fn standardised(p: &Population, field: &str) -> Result<Vec<f64>> {
    let mut x = Vec::with_capacity(p.humans.len());
    for &q in &p.humans {
        let v = opt_f64(p, field, q).with_context(|| {
            format!("human {}: no numeric field {field}", p.rows[p.primary[q]].id)
        })?;
        ensure!(v.is_finite(), "human {}: {field} is not finite", p.rows[p.primary[q]].id);
        x.push(v);
    }
    let mut dom: BTreeMap<&str, (f64, f64, f64)> = BTreeMap::new();
    for (k, &q) in p.humans.iter().enumerate() {
        let e = dom.entry(p.rows[p.primary[q]].domain.as_str()).or_default();
        let w = p.human_weights[k];
        e.0 += w;
        e.1 += w * x[k];
    }
    for (k, &q) in p.humans.iter().enumerate() {
        let e = dom.get_mut(p.rows[p.primary[q]].domain.as_str()).unwrap();
        let mu = e.1 / e.0;
        e.2 += p.human_weights[k] * (x[k] - mu).powi(2);
    }
    Ok(p.humans
        .iter()
        .enumerate()
        .map(|(k, &q)| {
            let e = dom[p.rows[p.primary[q]].domain.as_str()];
            let (mu, sd) = (e.1 / e.0, (e.2 / e.0).sqrt());
            if sd > 0.0 { (x[k] - mu) / sd } else { 0.0 }
        })
        .collect())
}

/// Weighted least-squares slope of y on z from sums (Σw, Σwz, Σwzz, Σwy, Σwzy).
pub fn slope_from_sums(s: &[f64; 5]) -> f64 {
    let den = s[2] - s[1] * s[1] / s[0];
    if den > 0.0 && s[0] > 0.0 {
        (s[4] - s[1] * s[3] / s[0]) / den
    } else {
        f64::NAN
    }
}

/// Weighted least-squares slope of y on z.
pub fn wls_slope(w: &[f64], z: &[f64], y: &[f64]) -> f64 {
    let mut s = [0.0; 5];
    for k in 0..w.len() {
        s[0] += w[k];
        s[1] += w[k] * z[k];
        s[2] += w[k] * z[k] * z[k];
        s[3] += w[k] * y[k];
        s[4] += w[k] * z[k] * y[k];
    }
    slope_from_sums(&s)
}

/// One seed's MWL slope (the margin statistic `mwl_slope`).
pub(super) fn mwl_slope_seed(p: &Population, field: &str, r: &SeedRun) -> Result<f64> {
    let z = standardised(p, field)?;
    let y: Vec<f64> = p.humans.iter().map(|&q| logit(r.inv[q])).collect();
    Ok(wls_slope(&p.human_weights, &z, &y))
}

pub(super) fn slope_gate(ctx: &Ctx, spec: &GateSpec) -> Result<Value> {
    let GateSpec::Slope {
        id,
        population,
        field,
        candidate,
        comparator,
        comparator_recipe,
        levels,
        margin,
    } = spec
    else {
        unreachable!()
    };
    let p = pop(ctx, population, id)?;
    ensure!(!p.humans.is_empty(), "gate {id}: population {population} has no humans");
    let z = standardised(p, field).with_context(|| format!("gate {id}"))?;
    let w = &p.human_weights;
    // Group sufficient statistics: w, wz, wzz per group, in unit-key order.
    let mut gk: BTreeMap<&str, Vec<usize>> = BTreeMap::new();
    for (k, &q) in p.humans.iter().enumerate() {
        gk.entry(p.units[q].as_str()).or_default().push(k);
    }
    let groups: Vec<Vec<usize>> = gk.into_values().collect();
    let gated = margin.is_some();
    let mut by_level = Map::new();
    let mut reasons = vec![];
    let mut all = true;
    for &level in levels {
        let comp = side(level, comparator, comparator_recipe.as_ref());
        let (yc, sa) = level_values(runs(ctx.book, population, candidate)?, level, ctx.cfg.released_seed, true, &logit_of)
            .with_context(|| format!("gate {id}: {candidate}"))?;
        let (yr, sb) = level_values(runs(ctx.book, population, comp)?, level, ctx.cfg.released_seed, false, &logit_of)
            .with_context(|| format!("gate {id}: {comp}"))?;
        let hc: Vec<f64> = p.humans.iter().map(|&q| yc[q]).collect();
        let hr: Vec<f64> = p.humans.iter().map(|&q| yr[q]).collect();
        let (bc, br) = (wls_slope(w, &z, &hc), wls_slope(w, &z, &hr));
        let gs: Vec<[f64; 7]> = groups
            .iter()
            .map(|g| {
                let mut s = [0.0; 7];
                for &k in g {
                    s[0] += w[k];
                    s[1] += w[k] * z[k];
                    s[2] += w[k] * z[k] * z[k];
                    s[3] += w[k] * hc[k];
                    s[4] += w[k] * z[k] * hc[k];
                    s[5] += w[k] * hr[k];
                    s[6] += w[k] * z[k] * hr[k];
                }
                s
            })
            .collect();
        let draws = bootstrap_draws(gs.len(), |ix| {
            let mut t = [0.0; 7];
            for &j in ix {
                for (a, b) in t.iter_mut().zip(&gs[j]) {
                    *a += b;
                }
            }
            slope_from_sums(&[t[0], t[1], t[2], t[3], t[4]]) - slope_from_sums(&[t[0], t[1], t[2], t[5], t[6]])
        });
        let point = bc - br;
        let (lower, upper) = match &draws {
            Some(d) if d.iter().all(|x| x.is_finite()) => (Some(d[24]), Some(d[974])),
            _ => (None, None),
        };
        let mut rec = json!({"comparator": comp, "seeds_candidate": sa, "seeds_comparator": sb, "humans": p.humans.len(),
            "source_groups": groups.len(), "slope_candidate": bc, "slope_comparator": br, "point": point,
            "lower": lower, "upper": upper});
        if let Some(m) = margin {
            match margin_value(ctx, m, level)? {
                Err(why) => {
                    reasons.push(why);
                    all = false;
                }
                Ok((mv, src)) => {
                    rec["margin"] = json!(mv);
                    rec["margin_source"] = json!(src);
                    match upper {
                        Some(u) if point.is_finite() => {
                            let pass = u <= mv;
                            rec["pass"] = json!(pass);
                            all &= pass;
                        }
                        _ => {
                            reasons.push(format!("{}: the slope or its interval is not finite (fewer than two groups, or no spread)", level.name()));
                            all = false;
                        }
                    }
                }
            }
        }
        by_level.insert(level.name().into(), rec);
    }
    Ok(json!({"id": id, "kind": "slope", "rule": SLOPE_RULE, "population": population, "field": field,
        "candidate": candidate, "difference": "slope_candidate_minus_slope_comparator", "levels": by_level,
        "status": status_of(&reasons, all, gated), "reasons": reasons}))
}

// ------------------------------------------------------------------ T3h (era no-worse)

/// `slop_ninja_cc_human_measure`'s within-domain slope (grammar-cc-human
/// `measure::within_slope`, copied byte for byte and pinned by a test): rows
/// are (stratum, year, value).
pub fn within_slope(rows: &[(usize, f64, f64)]) -> f64 {
    let mut m: HashMap<usize, (f64, f64, f64)> = HashMap::new();
    for &(s, y, v) in rows {
        let e = m.entry(s).or_default();
        e.0 += 1.0;
        e.1 += y;
        e.2 += v;
    }
    let (mut num, mut den) = (0.0, 0.0);
    for &(s, y, v) in rows {
        let e = m[&s];
        let (yb, vb) = (e.1 / e.0, e.2 / e.0);
        num += (y - yb) * (v - vb);
        den += (y - yb) * (y - yb);
    }
    if den > 0.0 { num / den } else { f64::NAN }
}

use std::collections::HashMap;

/// (domain index, year) per primary human, from the named integer year field.
fn era_keys(p: &Population, field: &str) -> Result<Vec<(usize, f64)>> {
    let doms: BTreeSet<&str> = p.humans.iter().map(|&q| p.rows[p.primary[q]].domain.as_str()).collect();
    let di: BTreeMap<&str, usize> = doms.iter().enumerate().map(|(i, d)| (*d, i)).collect();
    p.humans
        .iter()
        .map(|&q| {
            let r = &p.rows[p.primary[q]];
            let y = opt_f64(p, field, q)
                .filter(|y| y.is_finite() && y.fract() == 0.0)
                .with_context(|| format!("human {}: no integer year field {field}", r.id))?;
            Ok((di[r.domain.as_str()], y))
        })
        .collect()
}

/// One seed's |β| (the margin statistic `era_abs_slope`).
pub(super) fn era_abs_slope_seed(p: &Population, field: &str, r: &SeedRun) -> Result<f64> {
    let keys = era_keys(p, field)?;
    let rows: Vec<(usize, f64, f64)> = keys
        .iter()
        .zip(&p.humans)
        .map(|(&(d, y), &q)| (d, y, r.inv[q]))
        .collect();
    Ok(within_slope(&rows).abs())
}

pub(super) fn era_gate(ctx: &Ctx, spec: &GateSpec) -> Result<Value> {
    let GateSpec::EraSlope {
        id,
        population,
        field,
        candidate,
        comparator,
        comparator_recipe,
        levels,
        margin,
        logit,
    } = spec
    else {
        unreachable!()
    };
    let p = pop(ctx, population, id)?;
    ensure!(!p.humans.is_empty(), "gate {id}: population {population} has no humans");
    let keys = era_keys(p, field).with_context(|| format!("gate {id}"))?;
    let value: &dyn Fn(&SeedRun, usize) -> f64 = if *logit { &logit_of } else { &inv_of };
    let mut gk: BTreeMap<&str, Vec<usize>> = BTreeMap::new();
    for (k, &q) in p.humans.iter().enumerate() {
        gk.entry(p.units[q].as_str()).or_default().push(k);
    }
    let groups: Vec<Vec<usize>> = gk.into_values().collect();
    let gated = margin.is_some();
    let mut by_level = Map::new();
    let mut reasons = vec![];
    let mut all = true;
    for &level in levels {
        let comp = side(level, comparator, comparator_recipe.as_ref());
        let (vc, sa) = level_values(runs(ctx.book, population, candidate)?, level, ctx.cfg.released_seed, true, value)
            .with_context(|| format!("gate {id}: {candidate}"))?;
        let (vr, sb) = level_values(runs(ctx.book, population, comp)?, level, ctx.cfg.released_seed, false, value)
            .with_context(|| format!("gate {id}: {comp}"))?;
        let rows_of = |v: &[f64], ks: &mut dyn Iterator<Item = usize>| -> Vec<(usize, f64, f64)> {
            ks.map(|k| (keys[k].0, keys[k].1, v[p.humans[k]])).collect()
        };
        let all_k = || 0..p.humans.len();
        let bc = within_slope(&rows_of(&vc, &mut all_k()));
        let br = within_slope(&rows_of(&vr, &mut all_k()));
        let point = bc.abs() - br.abs();
        let draws = bootstrap_draws(groups.len(), |ix| {
            let mut it_c = ix.iter().flat_map(|&j| groups[j].iter().copied());
            let mut it_r = ix.iter().flat_map(|&j| groups[j].iter().copied());
            within_slope(&rows_of(&vc, &mut it_c)).abs() - within_slope(&rows_of(&vr, &mut it_r)).abs()
        });
        let (lower, upper) = match &draws {
            Some(d) if d.iter().all(|x| x.is_finite()) => (Some(d[24]), Some(d[974])),
            _ => (None, None),
        };
        let mut rec = json!({"comparator": comp, "seeds_candidate": sa, "seeds_comparator": sb, "humans": p.humans.len(),
            "source_groups": groups.len(), "beta_candidate": bc, "beta_comparator": br, "point": point,
            "lower": lower, "upper": upper});
        if let Some(m) = margin {
            match margin_value(ctx, m, level)? {
                Err(why) => {
                    reasons.push(why);
                    all = false;
                }
                Ok((mv, src)) => {
                    rec["margin"] = json!(mv);
                    rec["margin_source"] = json!(src);
                    match upper {
                        Some(u) if point.is_finite() => {
                            let pass = u <= mv;
                            rec["pass"] = json!(pass);
                            all &= pass;
                        }
                        _ => {
                            reasons.push(format!("{}: the slope or its interval is not finite", level.name()));
                            all = false;
                        }
                    }
                }
            }
        }
        by_level.insert(level.name().into(), rec);
    }
    Ok(json!({"id": id, "kind": "era_slope", "rule": ERA_RULE, "population": population, "field": field,
        "scale": if *logit { "uncalibrated_involvement_logit" } else { "involvement" },
        "candidate": candidate, "difference": "abs_beta_candidate_minus_abs_beta_comparator", "levels": by_level,
        "status": status_of(&reasons, all, gated), "reasons": reasons}))
}

// ------------------------------------------------------------------ T4a / T4b (B11's record)

/// T4a's and T4b's frozen bounds (§11.6, reading R14).
#[derive(Debug, Deserialize)]
#[serde(tag = "source", rename_all = "snake_case", deny_unknown_fields)]
pub enum SlackBound {
    /// T4b (mock): σ_m (V4, the manifest in force).
    SigmaM,
    /// T4a, block T: δ_m at n = 1,000 (V4).
    DeltaM,
    /// T4a, block C: δ_C at n = 4,000 from B12's σ record on Dev-iter-2 C
    /// (S1-4, item 12.4), bound by digest.
    DeltaC { record: PathBuf, sha256: String },
}

/// The bound each block's slack takes (structural; `validate`).
pub(super) fn check_slack_bound(id: &str, block: Block, b: &SlackBound) -> Result<()> {
    let (ok, want) = match block {
        Block::T => (matches!(b, SlackBound::DeltaM), "delta_m (T4a, block T)"),
        Block::C => (matches!(b, SlackBound::DeltaC { .. }), "delta_c (T4a, block C)"),
        Block::Mock => (matches!(b, SlackBound::SigmaM), "sigma_m (T4b, the mock)"),
    };
    ensure!(ok, "gate {id}: a block-{} slack takes the bound {want} (§11.6)", block.name());
    Ok(())
}

/// The export and CAL population B11's record must name for a block (§10).
fn slack_export(block: Block) -> (&'static str, &'static str) {
    match block {
        Block::T => ("product", "cal2_t"),
        Block::C => ("mainnet", "cal2_c"),
        Block::Mock => ("testnet", "cal2_mock"),
    }
}

/// A B12 σ record's design at `n` under 20/55/25 with the pinned bootstrap,
/// after checking its schema, digest and weight field.
fn sigma_design(path: &Path, sha256: &str, n: u64, weight_field: &str, what: &str) -> Result<(Value, Value)> {
    let b = read(path)?;
    let h = sha256_hex(&b);
    ensure!(h == sha256, "{what}: the sigma record {} hashes to {h}, not {sha256}", path.display());
    let v: Value = serde_json::from_slice(&b).with_context(|| format!("{what}: sigma record"))?;
    ensure!(v["schema"] == SIGMA_SCHEMA, "{what}: not a {SIGMA_SCHEMA} record (B12)");
    ensure!(
        v["bootstrap"]["as_pinned"] == true,
        "{what}: the sigma record's bootstrap is not the pinned one (item 12.3: 2,000 draws from seed 20261030)"
    );
    ensure!(
        v["inputs"]["weight_field"] == json!({"field": weight_field, "absent": "refused"}),
        "{what}: the sigma record's weight field is not {weight_field} (item 12.4)"
    );
    let ds: Vec<&Value> = v["designs"].as_array().into_iter().flatten().filter(|d| d["n"] == n).collect();
    let [d] = ds.as_slice() else {
        bail!("{what}: the sigma record has {} designs at n = {n}, not one", ds.len());
    };
    let mix: Option<Vec<f64>> = d["mix"].as_array().map(|m| m.iter().filter_map(Value::as_f64).collect());
    ensure!(
        mix.as_deref() == Some(&SIGMA_MIX[..]),
        "{what}: the n = {n} design's mix is not 20/55/25 (human_only, mixed, model_only)"
    );
    let d: Value = (*d).clone();
    Ok((v, d))
}

/// The bound's value and its source record.
fn slack_bound_value(id: &str, b: &SlackBound) -> Result<(f64, Value)> {
    Ok(match b {
        SlackBound::SigmaM => (SIGMA_M, json!({"source": "sigma_m", "value": SIGMA_M, "rule": "V4: sigma_m = 0.035 at n = 1,000 (the manifest in force); T4b's w/2 as a share of B0"})),
        SlackBound::DeltaM => (DELTA_M, json!({"source": "delta_m", "value": DELTA_M, "rule": "V4: delta_m = 0.070 at n = 1,000; T4a's block-T bound"})),
        SlackBound::DeltaC { record, sha256 } => {
            let what = format!("gate {id}");
            let (v, d) = sigma_design(record, sha256, SIGMA_N_C, "cohort_weight", &what)?;
            ensure!(v["inputs"]["block"] == "C", "{what}: delta_C's sigma record is not on block C (S1-4)");
            let sigma = d["sigma"].as_f64().filter(|x| x.is_finite() && *x >= 0.0).with_context(|| format!("{what}: sigma"))?;
            let delta = d["delta"].as_f64().with_context(|| format!("{what}: delta"))?;
            ensure!(
                delta.to_bits() == (2.0 * sigma).max(0.01).to_bits(),
                "{what}: the record's delta is not max(0.01, 2 sigma)"
            );
            (delta, json!({"source": "delta_c", "value": delta, "sigma_c": sigma, "record": record, "sha256": sha256,
                "rows_sha256": v["inputs"]["rows_sha256"], "n": SIGMA_N_C,
                "rule": "S1-4 (item 12.4): B12's delta = max(0.01, 2 sigma_C) at n = 4,000 under 20/55/25 on Dev-iter-2 C, cohort_weight, pinned bootstrap"}))
        }
    })
}

pub(super) fn slack_gate(ctx: &Ctx, spec: &GateSpec) -> Result<Value> {
    let GateSpec::Slack {
        id,
        population,
        system,
        record,
        record_sha256,
        bound,
        levels,
    } = spec
    else {
        unreachable!()
    };
    let p = pop(ctx, population, id)?;
    let b = read(record)?;
    let digest = sha256_hex(&b);
    ensure!(
        &digest == record_sha256,
        "gate {id}: the slack record {} hashes to {digest}, not {record_sha256}",
        record.display()
    );
    let v: Value = serde_json::from_slice(&b).with_context(|| format!("gate {id}: slack record"))?;
    ensure!(v["schema"] == SLACK_SCHEMA, "gate {id}: the slack record is not {SLACK_SCHEMA} (B11)");
    ensure!(
        v["population_rows_sha256"] == p.rows_sha256.as_str() && v["system"] == system.as_str(),
        "gate {id}: the slack record is not {system}'s on population {population}'s rows"
    );
    let (export, cal) = slack_export(p.block);
    ensure!(
        v["cross_fitted"] == true && v["export"] == export && v["cal_population"]["split"] == cal,
        "gate {id}: the slack record is not cross-fitted on the {export} export fitted on {cal} (B11; reading R14)"
    );
    let bound = match bound {
        Some(b) => {
            check_slack_bound(id, p.block, b)?;
            Some(slack_bound_value(id, b)?)
        }
        None => None,
    };
    let mut by_level = Map::new();
    let mut all = true;
    let mut reasons = vec![];
    for &level in levels {
        let seeds: Vec<u64> = match level {
            Level::Artifact => vec![ctx.cfg.released_seed],
            Level::Recipe => RECIPE_SEEDS.to_vec(),
        };
        let mut per = vec![];
        let mut pass = true;
        for s in seeds {
            match v["seeds"][s.to_string()]["slack_share_of_b0"].as_f64() {
                Some(x) if x.is_finite() => match &bound {
                    Some((bv, _)) => {
                        let ok = x <= *bv;
                        pass &= ok;
                        per.push(json!({"seed": s, "slack_share_of_b0": x, "pass": ok}));
                    }
                    None => per.push(json!({"seed": s, "slack_share_of_b0": x})),
                },
                _ => reasons.push(format!("{}: seed {s} has no finite slack_share_of_b0", level.name())),
            }
        }
        all &= pass;
        let mut rec = json!({"seeds": per});
        if bound.is_some() {
            rec["pass"] = json!(pass);
        }
        by_level.insert(level.name().into(), rec);
    }
    Ok(json!({"id": id, "kind": "slack", "population": population, "system": system,
        "bound": bound.as_ref().map(|b| b.1.clone()), "export": export, "cal_population": v["cal_population"],
        "record": record, "record_sha256": digest,
        "rule": "PROTOCOL-A2 §11.6 T4a/T4b with reading R14: B11's cross-fitted slack as a share of the system's own B0, at each seed of the level, <= the frozen bound (T4a: delta_m in T, delta_C in C; T4b: sigma_m); reported without a bound",
        "levels": by_level, "status": status_of(&reasons, all, bound.is_some()), "reasons": reasons}))
}

// ------------------------------------------------------------------ T8a's ratio, T8b and S3-1

/// T8a's ratio over ledger units on a subset of primary positions:
/// (point, per-unit weighted differences, per-unit weighted comparator losses).
pub fn ratio_units(p: &Population, pos: &[usize], lc: &[f64], lr: &[f64]) -> (f64, Vec<f64>, Vec<f64>) {
    let mut u: BTreeMap<&str, (f64, f64)> = BTreeMap::new();
    for &q in pos {
        let e = u.entry(p.units[q].as_str()).or_default();
        e.0 += p.weights[q] * (lc[q] - lr[q]);
        e.1 += p.weights[q] * lr[q];
    }
    let (d, r): (Vec<f64>, Vec<f64>) = u.into_values().unzip();
    (d.iter().sum::<f64>() / r.iter().sum::<f64>(), d, r)
}

/// The ratio's 1,000 sorted bootstrap draws (T8a's, one block).
pub fn ratio_draws(d: &[f64], r: &[f64]) -> Option<Vec<f64>> {
    bootstrap_draws(d.len(), |ix| {
        let (a, b) = ix.iter().fold((0.0, 0.0), |acc, &j| (acc.0 + d[j], acc.1 + r[j]));
        a / b
    })
}

pub(super) fn t8b_gate(ctx: &Ctx, spec: &GateSpec) -> Result<Value> {
    let GateSpec::T8b {
        id,
        population,
        candidate,
        comparator,
        comparator_recipe,
        levels,
        by,
    } = spec
    else {
        unreachable!()
    };
    let p = pop(ctx, population, id)?;
    ensure!(p.block == Block::Mock, "gate {id}: T8b runs on a mock population");
    let mut by_level = Map::new();
    for &level in levels {
        let comp = side(level, comparator, comparator_recipe.as_ref());
        let (lc, sa) = level_values(runs(ctx.book, population, candidate)?, level, ctx.cfg.released_seed, true, &loss_of)?;
        let (lr, sb) = level_values(runs(ctx.book, population, comp)?, level, ctx.cfg.released_seed, false, &loss_of)?;
        let one = |pos: &[usize]| {
            let (point, d, r) = ratio_units(p, pos, &lc, &lr);
            let dr = ratio_draws(&d, &r);
            json!({"documents": pos.len(), "ledger_units": d.len(), "point": point,
                "lower": dr.as_ref().map(|x| x[24]), "upper": dr.as_ref().map(|x| x[974])})
        };
        let all: Vec<usize> = (0..p.primary.len()).collect();
        let mut fields = Map::new();
        for f in by {
            let present_on_humans = p.humans.iter().any(|&q| opt_str(p, f, q).is_some());
            let mut vals: BTreeMap<String, Vec<usize>> = BTreeMap::new();
            for q in 0..p.primary.len() {
                if let Some(v) = opt_str(p, f, q) {
                    vals.entry(v.to_string()).or_default().push(q);
                }
            }
            let mut fm = Map::new();
            for (v, mut pos) in vals {
                if !present_on_humans {
                    pos.extend(p.humans.iter().copied().filter(|q| opt_str(p, f, *q).is_none()));
                    pos.sort();
                    pos.dedup();
                }
                fm.insert(v, one(&pos));
            }
            fields.insert(f.clone(), Value::Object(fm));
        }
        by_level.insert(level.name().into(), json!({"comparator": comp, "seeds_candidate": sa, "seeds_comparator": sb,
            "all": one(&all), "by": fields}));
    }
    Ok(json!({"id": id, "kind": "t8b", "population": population, "candidate": candidate,
        "rule": "PROTOCOL-A2 §11.3 T8b (reported): T8a's r = (B_cand - B_comp)/B_comp at the W_A8 weights, over the population and by each named row field (reading R9)",
        "levels": by_level, "status": "reported", "reasons": []}))
}

// ------------------------------------------------------------------ measures (S3-1, S3-4)

#[derive(Debug, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum MeasureSpec {
    /// S3-1 (§12.3): s on the Dev-iter-2 mock, with the branch and B_M.
    T8aS {
        id: String,
        population: String,
        candidate: String,
        comparator: String,
    },
    /// S3-4: the pooled within-domain SD of per-group paired differences.
    SliceSd {
        id: String,
        population: String,
        candidate: String,
        comparator: String,
    },
}

impl MeasureSpec {
    pub fn id(&self) -> &str {
        match self {
            MeasureSpec::T8aS { id, .. } | MeasureSpec::SliceSd { id, .. } => id,
        }
    }
    pub fn candidate(&self) -> &str {
        match self {
            MeasureSpec::T8aS { candidate, .. } | MeasureSpec::SliceSd { candidate, .. } => candidate,
        }
    }
}

/// S3-4's pooled within-domain SD of group values (reading R4): returns the
/// SD (None without a degree of freedom) and per-domain counts.
pub fn pooled_within_sd(values: &[(String, f64)]) -> (Option<f64>, Value) {
    let mut by: BTreeMap<&str, Vec<f64>> = BTreeMap::new();
    for (d, v) in values {
        by.entry(d.as_str()).or_default().push(*v);
    }
    let (mut ss, mut df) = (0.0, 0usize);
    let mut rec = Map::new();
    for (d, v) in &by {
        let m = v.iter().sum::<f64>() / v.len() as f64;
        let s: f64 = v.iter().map(|x| (x - m).powi(2)).sum();
        ss += s;
        df += v.len() - 1;
        rec.insert((*d).into(), json!({"source_groups": v.len(), "mean": m, "sum_squares": s}));
    }
    ((df > 0).then(|| (ss / df as f64).sqrt()), json!({"domains": rec, "degrees_of_freedom": df}))
}

pub(super) fn measure(ctx: &Ctx, m: &MeasureSpec) -> Result<Value> {
    match m {
        MeasureSpec::T8aS {
            id,
            population,
            candidate,
            comparator,
        } => {
            let p = pop(ctx, population, id)?;
            ensure!(p.block == Block::Mock, "measure {id}: s is measured on a mock population");
            let (lc, sa) = level_values(runs(ctx.book, population, candidate)?, Level::Artifact, ctx.cfg.released_seed, true, &loss_of)?;
            let (lr, sb) = level_values(runs(ctx.book, population, comparator)?, Level::Artifact, ctx.cfg.released_seed, false, &loss_of)?;
            let all: Vec<usize> = (0..p.primary.len()).collect();
            let (point, d, r) = ratio_units(p, &all, &lc, &lr);
            let dr = ratio_draws(&d, &r).with_context(|| format!("measure {id}: fewer than two ledger units"))?;
            let se = sample_sd(&dr).context("bootstrap SE")?;
            let ess = kish_ess(&p.weights);
            let s = se * (ess / S_DOCS).sqrt();
            let (branch, b_m, shortfall) = t8a_branch(s)?;
            Ok(json!({"id": id, "kind": "t8a_s", "value": "S3-1", "population": population, "candidate": candidate,
                "comparator": comparator, "seeds_candidate": sa, "seeds_comparator": sb, "ledger_units": d.len(),
                "r_point": point, "bootstrap_se": se, "kish_ess": ess, "s": s,
                "rule": "PROTOCOL-A2 §12.3: s = the paired SE of r per 1,000 self-weighted documents; the bootstrap SE scaled by sqrt(ESS/1,000) (reading R5)",
                "t8a_branch": {"branch": branch.name(), "b_m": b_m, "s_above_0_19_shortfall": shortfall, "rule": "r2_mock_batches_v2 on s's printed decimal"}}))
        }
        MeasureSpec::SliceSd {
            id,
            population,
            candidate,
            comparator,
        } => {
            let p = pop(ctx, population, id)?;
            ensure!(
                p.block == Block::T && p.form == Some(Form::N),
                "measure {id}: S3-4 is measured on a T-block N-form population"
            );
            let (la, sa) = level_values(runs(ctx.book, population, candidate)?, Level::Artifact, ctx.cfg.released_seed, true, &loss_of)?;
            let (lb, sb) = level_values(runs(ctx.book, population, comparator)?, Level::Artifact, ctx.cfg.released_seed, false, &loss_of)?;
            let mut g: BTreeMap<&str, (f64, BTreeSet<&str>)> = BTreeMap::new();
            for q in 0..p.primary.len() {
                let e = g.entry(p.units[q].as_str()).or_default();
                e.0 += p.weights[q] * (la[q] - lb[q]);
                e.1.insert(p.rows[p.primary[q]].domain.as_str());
            }
            let n = g.len() as f64;
            let mut vals = vec![];
            for (u, (s, doms)) in &g {
                ensure!(doms.len() == 1, "measure {id}: source group {u} spans domains {doms:?}");
                vals.push((doms.iter().next().unwrap().to_string(), n * s));
            }
            let (sd, rec) = pooled_within_sd(&vals);
            Ok(json!({"id": id, "kind": "slice_sd", "value": "S3-4", "population": population, "candidate": candidate,
                "comparator": comparator, "seeds_candidate": sa, "seeds_comparator": sb, "source_groups": vals.len(),
                "sd": sd, "by_domain": rec,
                "rule": "PROTOCOL-A2 §12.6 S3-4: the pooled within-domain SD of per-group paired differences (reading R4)"}))
        }
    }
}

// ------------------------------------------------------------------ T2: slice plan (S4-1) and gate

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SlicePlanConfig {
    pub schema: String,
    /// The T-block and the C-block N-form populations (Test-2 T and C).
    pub populations: Vec<PopulationSpec>,
    /// S3-4's SD, read by digest from the B10 evaluation that measured it.
    pub sd: SdSource,
    #[serde(default)]
    pub held_out_generators: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SdSource {
    /// A B10 evaluation record holding the `slice_sd` measure.
    pub record: PathBuf,
    pub sha256: String,
    /// The measure's id in that record.
    pub measure: String,
}

pub fn parse_slice_plan_config(bytes: &[u8]) -> Result<SlicePlanConfig> {
    let c: SlicePlanConfig = serde_json::from_slice(bytes).context("slice plan configuration")?;
    ensure!(
        c.schema == SLICE_PLAN_CONFIG_SCHEMA,
        "slice plan configuration schema {} is not {SLICE_PLAN_CONFIG_SCHEMA}",
        c.schema
    );
    Ok(c)
}

/// S3-4's SD from the B10 evaluation record that measured it: the named
/// `slice_sd` measure on a Dev-iter-2 T population, finite and positive.
pub fn read_sd(record: &Path, sha256: &str, measure: &str) -> Result<(f64, Value)> {
    let b = read(record)?;
    let h = sha256_hex(&b);
    ensure!(h == sha256, "S3-4's record {} hashes to {h}, not {sha256}", record.display());
    let v: Value = serde_json::from_slice(&b).context("S3-4's record")?;
    ensure!(v["schema"] == EVAL_SCHEMA, "S3-4's record is not a B10 evaluation ({EVAL_SCHEMA})");
    let m = v["measures"]
        .as_array()
        .and_then(|ms| ms.iter().find(|m| m["id"] == measure))
        .with_context(|| format!("S3-4's record has no measure {measure}"))?;
    ensure!(m["kind"] == "slice_sd", "measure {measure} is not S3-4's slice_sd");
    let pid = m["population"].as_str().context("S3-4's population")?;
    ensure!(
        v["populations"][pid]["split"] == "deviter2_t",
        "S3-4 is measured on Dev-iter-2 T (split deviter2_t), not on population {pid}"
    );
    let sd = m["sd"]
        .as_f64()
        .filter(|x| x.is_finite() && *x > 0.0)
        .context("S3-4's SD is not finite and positive")?;
    Ok((
        sd,
        json!({"value": sd, "record": record, "sha256": h, "measure": measure, "population": pid,
            "candidate": m["candidate"], "comparator": m["comparator"]}),
    ))
}

/// Primary positions of a comp_eval slice of the population (primary rows only).
fn slice_positions(p: &Population, hog: &[String], dimension: &str, value: &str) -> Result<Vec<usize>> {
    let opts = comp_eval::Options {
        held_out_generators: hog.to_vec(),
        ..comp_eval::Options::default()
    };
    let at: BTreeMap<usize, usize> = p.primary.iter().enumerate().map(|(q, &i)| (i, q)).collect();
    Ok(match build_slices(&p.rows, &opts)
        .into_iter()
        .find(|s| s.dimension == dimension && s.value == value)
    {
        None => vec![],
        Some(s) => {
            ensure!(!s.non_primary, "slice {dimension}:{value} holds rows outside the primary population");
            s.idx.iter().filter_map(|i| at.get(i).copied()).collect()
        }
    })
}

/// Pipeline counts of a slice's terms (documents, source groups, humans, frontier positives).
fn slice_counts(terms: &[(&Population, Vec<usize>)]) -> Value {
    let (mut docs, mut groups, mut humans, mut frontier) = (0, 0, 0, 0);
    for (p, pos) in terms {
        let g: BTreeSet<&str> = pos.iter().map(|&q| p.rows[p.primary[q]].group.as_str()).collect();
        docs += pos.len();
        groups += g.len();
        humans += pos.iter().filter(|&&q| p.rows[p.primary[q]].is_human()).count();
        frontier += pos.iter().filter(|&&q| p.rows[p.primary[q]].population == FRONTIER_POPULATION).count();
    }
    json!({"documents": docs, "source_groups": groups, "human_documents": humans, "frontier_positives": frontier})
}

fn slice_shortfall(counts: &Value, frontier_slice: bool) -> Vec<String> {
    let g = counts["source_groups"].as_u64().unwrap_or(0) as usize;
    let h = counts["human_documents"].as_u64().unwrap_or(0) as usize;
    let f = counts["frontier_positives"].as_u64().unwrap_or(0) as usize;
    let mut s = vec![];
    if g < SLICE_MIN_GROUPS {
        s.push(format!("source_groups {g} < {SLICE_MIN_GROUPS}"));
    }
    if h < SLICE_MIN_HUMANS {
        s.push(format!("human_documents {h} < {SLICE_MIN_HUMANS}"));
    }
    if frontier_slice && f < SLICE_MIN_FRONTIER_POSITIVES {
        s.push(format!("frontier_positives {f} < {SLICE_MIN_FRONTIER_POSITIVES}"));
    }
    s
}

/// One slice of §11.4's family: its id and terms (population, dimension, value, coefficient).
pub struct SliceDef<'a> {
    pub id: String,
    pub terms: Vec<(&'a Population, &'static str, String, f64)>,
}

/// §11.4's candidate family, derived from the rows: every T-block domain
/// (`domain:<d>`), the CC strata essays and news (`stratum:<s>`), and
/// `held_out_generator` and `frontier`, T and C pooled at half weight.
pub fn slice_family<'a>(t: &'a Population, c: &'a Population) -> Vec<SliceDef<'a>> {
    let doms: BTreeSet<&str> = t.primary.iter().map(|&i| t.rows[i].domain.as_str()).collect();
    let mut out: Vec<SliceDef> = doms
        .into_iter()
        .map(|d| SliceDef { id: format!("domain:{d}"), terms: vec![(t, "domain", d.to_string(), 1.0)] })
        .collect();
    for s in C_SLICE_STRATA {
        out.push(SliceDef { id: format!("stratum:{s}"), terms: vec![(c, "domain", s.to_string(), 1.0)] });
    }
    for d in POOLED_SLICES {
        out.push(SliceDef {
            id: d.to_string(),
            terms: vec![(t, d, "all".into(), POOLED_COEF), (c, d, "all".into(), POOLED_COEF)],
        });
    }
    out
}

/// The plan's slices, K and z_K from the family, the rows and S3-4's SD
/// (shared by S4-1's writer and T2's re-derivation).
fn plan_slices(t: &Population, c: &Population, hog: &[String], sd: f64) -> Result<(Vec<Value>, usize, Option<f64>)> {
    let mut out = vec![];
    let mut se = vec![];
    for sl in slice_family(t, c) {
        let mut terms = vec![];
        for (p, dim, val, _) in &sl.terms {
            terms.push((*p, slice_positions(p, hog, dim, val)?));
        }
        let counts = slice_counts(&terms);
        let short = slice_shortfall(&counts, sl.id == "frontier");
        let candidate = short.is_empty();
        let sep = sd / (counts["source_groups"].as_u64().unwrap_or(0) as f64).sqrt();
        if candidate {
            se.push(sep);
        }
        out.push(json!({"id": sl.id,
            "terms": sl.terms.iter().map(|(p, d, v, k)| json!({"population": p.id, "dimension": d, "value": v, "coef": k})).collect::<Vec<_>>(),
            "counts": counts, "candidate": candidate, "shortfall": short, "se_planned": candidate.then_some(sep)}));
    }
    let (k, z, worthy) = slice_gate_k(&se);
    let mut w = worthy.into_iter();
    for s in &mut out {
        let gw = s["candidate"] == true && w.next().unwrap_or(false);
        s["gate_worthy"] = json!(gw);
        s["gated"] = json!(gw);
    }
    Ok((out, k, z))
}

/// The T and C populations of a slice plan: one N-form population per block.
fn plan_pair<'a>(pops: &[&'a Population]) -> Result<(&'a Population, &'a Population)> {
    let one = |b: Block| -> Result<&'a Population> {
        let v: Vec<&&Population> = pops.iter().filter(|p| p.block == b).collect();
        let [p] = v.as_slice() else {
            bail!("a slice plan holds exactly one block-{} population, not {}", b.name(), v.len());
        };
        ensure!(p.form == Some(Form::N) && p.probe.is_none(), "slice population {} is not N-form", p.id);
        Ok(p)
    };
    ensure!(pops.len() == 2, "a slice plan holds the T and the C N-form populations only");
    Ok((one(Block::T)?, one(Block::C)?))
}

/// S4-1: the slice plan from pipeline counts and S3-4's SD. Reads rows only.
pub fn slice_plan(cfg: &SlicePlanConfig) -> Result<Value> {
    let (sd, sd_rec) = read_sd(&cfg.sd.record, &cfg.sd.sha256, &cfg.sd.measure)?;
    let mut pops = BTreeMap::new();
    for s in &cfg.populations {
        ensure!(s.probe.is_none(), "population {}: a twin population is not a slice population", s.id);
        let p = load_population(s, &BTreeSet::new(), &BTreeSet::new())
            .with_context(|| format!("population {}", s.id))?;
        ensure!(pops.insert(s.id.clone(), p).is_none(), "population {} named twice", s.id);
    }
    let (t, c) = plan_pair(&pops.values().collect::<Vec<_>>())?;
    let (slices, k, z) = plan_slices(t, c, &cfg.held_out_generators, sd)?;
    Ok(json!({
        "schema": SLICE_PLAN_SCHEMA, "protocol": PROTOCOL, "rule": SLICE_PLAN_RULE,
        "minimums": {"source_groups": SLICE_MIN_GROUPS, "human_documents": SLICE_MIN_HUMANS, "frontier_positives": SLICE_MIN_FRONTIER_POSITIVES},
        "sd": sd_rec, "held_out_generators": cfg.held_out_generators,
        "t_population": t.id, "c_population": c.id,
        "populations": pops.iter().map(|(k, p)| (k.clone(), json!({"split": p.split, "block": p.block.name(), "rows_jsonl_sha256": p.rows_sha256}))).collect::<Map<_, _>>(),
        "slices": slices, "k": k, "z_k": z, "slice_bound": SLICE_BOUND, "alpha": SLICE_ALPHA,
        "read": "rows metadata only (pipeline counts); no prediction is read",
    }))
}

/// T2's pass rule at a gated slice (§11.4).
pub fn slice_pass(statistic: f64, z_k: f64) -> bool {
    statistic <= z_k
}

pub(super) fn slices_gate(ctx: &Ctx, spec: &GateSpec) -> Result<Value> {
    let GateSpec::Slices {
        id,
        plan,
        plan_sha256,
        candidate,
        comparator,
        comparator_recipe,
        levels,
    } = spec
    else {
        unreachable!()
    };
    let b = read(plan)?;
    let digest = sha256_hex(&b);
    ensure!(&digest == plan_sha256, "gate {id}: the slice plan {} hashes to {digest}, not {plan_sha256}", plan.display());
    let pl: Value = serde_json::from_slice(&b).context("slice plan")?;
    ensure!(pl["schema"] == SLICE_PLAN_SCHEMA, "gate {id}: not a {SLICE_PLAN_SCHEMA} record");
    let mut pps = vec![];
    for (pid, rec) in pl["populations"].as_object().context("plan populations")? {
        let p = pop(ctx, pid, id)?;
        ensure!(
            rec["rows_jsonl_sha256"] == p.rows_sha256.as_str(),
            "gate {id}: population {pid}'s rows are not the rows the slice plan counted"
        );
        pps.push(p);
    }
    let (t, c) = plan_pair(&pps).with_context(|| format!("gate {id}"))?;
    ensure!(
        pl["t_population"] == t.id.as_str() && pl["c_population"] == c.id.as_str(),
        "gate {id}: the slice plan's T and C populations are not its populations"
    );
    // S3-4's SD, re-read from its record by digest.
    let (sd, _) = read_sd(
        Path::new(pl["sd"]["record"].as_str().context("plan sd record")?),
        pl["sd"]["sha256"].as_str().context("plan sd sha256")?,
        pl["sd"]["measure"].as_str().context("plan sd measure")?,
    )
    .with_context(|| format!("gate {id}"))?;
    ensure!(
        pl["sd"]["value"].as_f64().map(f64::to_bits) == Some(sd.to_bits()),
        "gate {id}: the slice plan's SD is not S3-4's record's"
    );
    let hog: Vec<String> = serde_json::from_value(pl["held_out_generators"].clone()).context("held_out_generators")?;
    // The family, counts, shortfalls, candidates, planned SEs, K, z_K and the
    // gated set are re-derived from the rows and S3-4's SD; the plan must equal them.
    let (slices, k, z) = plan_slices(t, c, &hog, sd)?;
    ensure!(
        Value::Array(slices.clone()) == pl["slices"]
            && pl["k"].as_u64() == Some(k as u64)
            && pl["z_k"].as_f64().map(f64::to_bits) == z.map(f64::to_bits),
        "gate {id}: the slice plan does not follow from the rows, §11.4's family and S3-4's SD (its slices, K, z_K or gated set differ)"
    );
    let gated_n = slices.iter().filter(|s| s["gated"] == true).count();
    let mut out = vec![];
    let mut reasons = vec![];
    let mut all = true;
    for (s, sl) in slices.iter().zip(slice_family(t, c)) {
        let gated = s["gated"] == true;
        let mut terms = vec![];
        for (p, dim, val, coef) in &sl.terms {
            terms.push((*p, slice_positions(p, &hog, dim, val)?, *coef));
        }
        let mut by_level = Map::new();
        for &level in levels {
            let comp = side(level, comparator, comparator_recipe.as_ref());
            let mut ts = vec![];
            for (p, pos, coef) in &terms {
                if pos.is_empty() {
                    continue;
                }
                let (la, _) = level_values(runs(ctx.book, &p.id, candidate)?, level, ctx.cfg.released_seed, true, &loss_of)?;
                let (lb, _) = level_values(runs(ctx.book, &p.id, comp)?, level, ctx.cfg.released_seed, false, &loss_of)?;
                ts.push(Term {
                    block: p.block,
                    units: pos.iter().map(|&q| p.units[q].as_str()).collect(),
                    weights: subset_weights(p, pos)?,
                    delta: pos.iter().map(|&q| la[q] - lb[q]).collect(),
                    coef: *coef,
                });
            }
            let mut rec = json!({"comparator": comp});
            if ts.is_empty() {
                rec["point"] = Value::Null;
            } else {
                let (point, blocks) = composite_units(&ts);
                let (lower, upper, se) = interval(&blocks);
                rec = json!({"comparator": comp, "units": units_json(&blocks), "point": point, "lower": lower,
                    "upper": upper, "se_realised": se});
                match se {
                    Some(se) if se.is_finite() && se > 0.0 => {
                        let stat = point / se;
                        rec["statistic"] = json!(stat);
                        if gated {
                            let pass = slice_pass(stat, z.context("a gated slice without z_K")?);
                            rec["pass"] = json!(pass);
                            all &= pass;
                        }
                    }
                    _ if gated => {
                        reasons.push(format!("slice {} {}: SE_realised is 0 or not finite", s["id"], level.name()));
                        all = false;
                    }
                    _ => {}
                }
            }
            if gated && ts.is_empty() {
                reasons.push(format!("slice {} {}: no documents", s["id"], level.name()));
                all = false;
            }
            by_level.insert(level.name().into(), rec);
        }
        out.push(json!({"slice": s["id"], "gated": gated, "counts": s["counts"], "se_planned": s["se_planned"], "levels": by_level}));
    }
    let status = if !reasons.is_empty() {
        "not_evaluated"
    } else if all {
        "pass"
    } else {
        "fail"
    };
    Ok(json!({"id": id, "kind": "slices", "rule": T2_RULE, "slice_rule": SLICE_RULE, "plan": plan, "plan_sha256": digest,
        "k": k, "z_k": z, "candidate": candidate, "gated_slices": gated_n, "sd": pl["sd"],
        "note": if k == 0 { "no candidate slice: K = 0 and T2 gates nothing (amendment 15 item 15.3)" } else { "" },
        "slices": out, "status": status, "reasons": reasons}))
}

// ------------------------------------------------------------------ null check (§9.1 step 4, item 1.10)

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProtocolGate {
    pub protocol: String,
    pub gate: String,
    /// The block the configuration gate evaluates (the null check's subset
    /// gates are per block; reading R11).
    #[serde(default)]
    pub block: Option<Block>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NullPlan {
    pub gates: Vec<ProtocolGate>,
    #[serde(default)]
    pub planned: Option<PathBuf>,
    #[serde(default)]
    pub planned_sha256: Option<String>,
}

/// The blocks each subset gate is evaluated on (§11.2, §11.5, §11.7, item 1.8).
pub fn null_subset_blocks(protocol: &str) -> &'static [Block] {
    match protocol {
        "T1a" | "T1e" | "T3b" => &[Block::T],
        "T5d" => &[Block::C],
        "T1c" | "T3a" | "T3c" | "T3e" => &[Block::T, Block::C],
        _ => &[],
    }
}

/// B13's key for a subset gate on a block.
pub fn planned_key(protocol: &str, block: Block) -> String {
    format!("{protocol}.{}", block.name())
}

fn gate_by<'a>(gates: &'a [Value], id: &str) -> Result<&'a Value> {
    gates.iter().find(|g| g["id"] == id).with_context(|| format!("unknown gate {id}"))
}

fn spec_by<'a>(cfg: &'a Config, id: &str, what: &str) -> Result<&'a GateSpec> {
    cfg.gates.iter().find(|g| g.id() == id).with_context(|| format!("{what} names unknown gate {id}"))
}

/// The blocks of the populations a configuration gate reads.
fn spec_blocks(cfg: &Config, g: &GateSpec) -> BTreeSet<Block> {
    g.populations()
        .iter()
        .filter_map(|p| cfg.populations.iter().find(|x| x.id == *p).map(|x| x.block))
        .collect()
}

/// Structural checks of the null-check plan (before any read).
pub(super) fn validate_null_plan(cfg: &Config, np: &NullPlan) -> Result<()> {
    let mut seen = BTreeSet::new();
    for e in &np.gates {
        let g = spec_by(cfg, &e.gate, "the null-check plan")?;
        let subset = NULL_SUBSET.contains(&e.protocol.as_str());
        ensure!(
            subset || NULL_REPORTED.contains(&e.protocol.as_str()),
            "null-check plan: {} is neither in item 1.10's subset nor reported",
            e.protocol
        );
        if subset {
            let b = e
                .block
                .with_context(|| format!("null-check plan: subset gate {} ({}) names no block", e.gate, e.protocol))?;
            ensure!(
                null_subset_blocks(&e.protocol).contains(&b),
                "null-check plan: {} is not evaluated on block {}",
                e.protocol,
                b.name()
            );
            ensure!(
                spec_blocks(cfg, g) == BTreeSet::from([b]),
                "null-check plan: gate {} does not read block {} only",
                e.gate,
                b.name()
            );
            ensure!(
                g.gated(),
                "null-check plan: subset gate {} ({}) has no pass rule (item 1.10's subset gates are gated)",
                e.gate,
                e.protocol
            );
            ensure!(seen.insert(planned_key(&e.protocol, b)), "null-check plan: {} twice", planned_key(&e.protocol, b));
        }
    }
    Ok(())
}

/// The null-check record: per gate and block of item 1.10's subset, its
/// status and the margin-rule faults (a)–(c); the reported gates; the pass
/// rate beside B13's joint pass probability at zero.
pub(super) fn null_check_record(np: &NullPlan, gates: &[Value]) -> Result<Value> {
    let planned = match (&np.planned, &np.planned_sha256) {
        (Some(p), Some(h)) => {
            let b = read(p)?;
            ensure!(&sha256_hex(&b) == h, "the planned-values file {} is not {h}", p.display());
            let v: Value = serde_json::from_slice(&b).context("planned values")?;
            ensure!(v["schema"] == PLANNED_SCHEMA, "the planned-values file is not {PLANNED_SCHEMA} (B13)");
            ensure!(
                v["joint_pass_probability_at_zero"].as_f64().is_some_and(|x| (0.0..=1.0).contains(&x)),
                "the planned-values file has no joint_pass_probability_at_zero in [0, 1] (item 1.10 (a))"
            );
            Some(v)
        }
        (None, None) => None,
        _ => bail!("planned and planned_sha256 come together"),
    };
    let mut per = vec![];
    let (mut evaluated, mut passed, mut not_evaluated, mut faults_n) = (0, 0, 0, 0);
    let mut missing_planned = vec![];
    let mut named = BTreeSet::new();
    for e in &np.gates {
        let g = gate_by(gates, &e.gate)?;
        let subset = NULL_SUBSET.contains(&e.protocol.as_str());
        let status = g["status"].as_str().unwrap_or("");
        let mut faults = vec![];
        let key = e.block.map(|b| planned_key(&e.protocol, b));
        if subset {
            let key = key.clone().context("a subset gate without its block")?;
            named.insert(key.clone());
            for r in g["reasons"].as_array().into_iter().flatten() {
                let r = r.as_str().unwrap_or("");
                if r.contains("margin") {
                    faults.push(format!("(a) {r}"));
                }
            }
            let pl = planned.as_ref().map(|v| &v["gates"][&key]);
            let p0 = pl.and_then(|v| v["pass_probability_at_zero"].as_f64());
            let se = pl.and_then(|v| v["planned_se"].as_f64());
            if p0.is_none() {
                missing_planned.push(key.clone());
            }
            if let Some(p0) = p0
                && p0 >= NULL_PLANNED_PASS
                && status == "fail"
            {
                faults.push(format!("(b) planned pass probability at 0 is {p0} >= {NULL_PLANNED_PASS} and the gate fails"));
            }
            let point = g.pointer("/levels/artifact/point").and_then(Value::as_f64);
            if let (Some(pt), Some(se)) = (point, se)
                && pt.abs() > NULL_SE_MULTIPLE * se
            {
                faults.push(format!("(c) point {pt} lies outside +-{NULL_SE_MULTIPLE} planned SEs ({se}) of 0"));
            }
            match status {
                "pass" | "fail" => {
                    evaluated += 1;
                    passed += (status == "pass") as usize;
                }
                _ => not_evaluated += 1,
            }
            faults_n += faults.len();
        }
        per.push(json!({"protocol": e.protocol, "block": e.block.map(Block::name), "key": key, "gate": e.gate,
            "role": if subset { "subset" } else { "reported" }, "status": status,
            "point": g.pointer("/levels/artifact/point"), "faults": faults}));
    }
    let missing: Vec<String> = NULL_SUBSET
        .iter()
        .flat_map(|p| null_subset_blocks(p).iter().map(move |b| planned_key(p, *b)))
        .filter(|k| !named.contains(k))
        .collect();
    Ok(json!({"rule": "PROTOCOL-A2 §9.1 step 4 as amended by item 1.10: Faux 29 as the artifact-level candidate against R0, margins from Faux 17/29/41; the subset T1a, T1c, T1e, T3a, T3b, T3c, T3e and T5d is recorded per gate and block (reading R11); T1b, T5b, T5e, T3f and T3g are reported; a margin-rule fault (a)-(c) is fixed by forward amendment before any arm fit and the null check re-run",
        "gates": per, "subset_evaluated": evaluated, "subset_passed": passed, "subset_not_evaluated": not_evaluated,
        "pass_rate": (evaluated > 0).then(|| passed as f64 / evaluated as f64),
        "joint_pass_probability_at_zero": planned.as_ref().map(|v| v["joint_pass_probability_at_zero"].clone()),
        "joint_pass_probability_source": "B13 (a), item 1.10 and S1-5: recorded beside the pass rate",
        "subset_missing": missing, "planned": np.planned, "planned_missing": missing_planned,
        "faults": faults_n, "amendment_required": faults_n > 0}))
}

// ------------------------------------------------------------------ selection (§9.2 steps 5–9)

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CandKind {
    Retrained,
    CalibrationOnly,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CondKind {
    HumanOnly,
    RForm,
    HInj,
    Fs,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SelCond {
    pub condition: CondKind,
    pub gate: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SelCand {
    pub name: String,
    pub kind: CandKind,
    pub blend: String,
    pub conditions: Vec<SelCond>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SelectionSpec {
    pub record: PathBuf,
    pub order: Vec<String>,
    pub candidates: Vec<SelCand>,
}

/// Structural checks of a selection (before any read).
pub(super) fn validate_selection(cfg: &Config, s: &SelectionSpec) -> Result<()> {
    let names: BTreeSet<&str> = s.candidates.iter().map(|c| c.name.as_str()).collect();
    ensure!(names.len() == s.candidates.len(), "selection names a candidate twice");
    let order: BTreeSet<&str> = s.order.iter().map(String::as_str).collect();
    ensure!(order == names && order.len() == s.order.len(), "the selection order must list every candidate once");
    for c in &s.candidates {
        ensure!(cfg.blends.iter().any(|b| b.id == c.blend), "selection {}: unknown blend {}", c.name, c.blend);
        let has = |k: CondKind| c.conditions.iter().any(|x| x.condition == k);
        ensure!(has(CondKind::HumanOnly) && has(CondKind::RForm), "selection {}: needs the human-only and R-form conditions (§9.2 step 5)", c.name);
        if c.kind == CandKind::Retrained {
            ensure!(has(CondKind::HInj) && has(CondKind::Fs), "selection {}: a retrained candidate also needs the H-inj and FS conditions", c.name);
        }
        for x in &c.conditions {
            let g = spec_by(cfg, &x.gate, &format!("selection {}", c.name))?;
            let ok = matches!(
                (x.condition, g),
                (CondKind::HumanOnly, GateSpec::Paired { statistic: Statistic::HumanBrierBalanced, margin: Some(_), .. })
                    | (CondKind::RForm, GateSpec::Paired { statistic: Statistic::Brier, margin: Some(_), .. })
                    | (CondKind::HInj, GateSpec::Shift { margin: Some(_), .. })
                    | (CondKind::Fs, GateSpec::Fs { margin: Some(_), .. })
            );
            ensure!(ok, "selection {}: gate {} is not a {:?} condition", c.name, x.gate, x.condition);
        }
    }
    Ok(())
}

/// The tie interval between two blends (reading R10), candidate a − b.
fn tie_interval(ctx: &Ctx, a: &BlendSpec, b: &BlendSpec) -> Result<Value> {
    let mut ts = vec![];
    for (ta, tb, coef) in [(&a.t_n, &b.t_n, 0.25), (&a.t_r, &b.t_r, 0.25), (&a.c_n, &b.c_n, 0.5)] {
        ensure!(ta.population == tb.population, "tie: the blends' populations differ");
        let p = &ctx.pops[&ta.population];
        let (la, _) = level_values(runs(ctx.book, &ta.population, &ta.system)?, Level::Recipe, ctx.cfg.released_seed, true, &loss_of)?;
        let (lb, _) = level_values(runs(ctx.book, &tb.population, &tb.system)?, Level::Recipe, ctx.cfg.released_seed, true, &loss_of)?;
        ts.push(Term {
            block: p.block,
            units: p.units.iter().map(String::as_str).collect(),
            weights: p.weights.clone(),
            delta: la.iter().zip(&lb).map(|(x, y)| x - y).collect(),
            coef,
        });
    }
    let (point, blocks) = composite_units(&ts);
    let (lower, upper, _) = interval(&blocks);
    Ok(json!({"point": point, "lower": lower, "upper": upper, "units": units_json(&blocks),
        "includes_zero": lower.zip(upper).map(|(l, u)| l <= 0.0 && 0.0 <= u)}))
}

/// Eligibility, U7 and the tie rule: the summary and `selection-record-r2.json`'s
/// bytes, which the run writes last, just before the evaluation.
pub(super) fn selection(ctx: &Ctx, s: &SelectionSpec, gates: &[Value], blends: &Value) -> Result<(Value, Vec<u8>)> {
    let mut cands = vec![];
    let mut eligible = vec![];
    for c in &s.candidates {
        let mut conds = vec![];
        let mut ok = true;
        for x in &c.conditions {
            let g = gate_by(gates, &x.gate)?;
            let exempt = c.kind == CandKind::CalibrationOnly && matches!(x.condition, CondKind::HInj | CondKind::Fs);
            let pass = g["status"] == "pass";
            if !exempt {
                ok &= pass;
            }
            conds.push(json!({"condition": format!("{:?}", x.condition), "gate": x.gate, "status": g["status"],
                "role": if exempt { "reported (R0-recal exempt, §9.2 step 5)" } else { "required" },
                "levels": g["levels"], "reasons": g["reasons"]}));
        }
        let mean = blends[&c.blend]["seed_mean"].as_f64().with_context(|| format!("blend {} has no seed mean", c.blend))?;
        let pos = s.order.iter().position(|o| *o == c.name).unwrap();
        if ok {
            eligible.push((mean, pos, c));
        }
        cands.push(json!({"name": c.name, "kind": format!("{:?}", c.kind), "blend": c.blend, "blend_seed_mean": mean,
            "blend_values": blends[&c.blend], "conditions": conds, "eligible": ok}));
    }
    eligible.sort_by(|a, b| a.0.total_cmp(&b.0).then(a.1.cmp(&b.1)));
    let ranking: Vec<&str> = eligible.iter().map(|e| e.2.name.as_str()).collect();
    let (winner, tie) = match eligible.len() {
        0 => (None, Value::Null),
        1 => (Some(eligible[0].2.name.as_str()), Value::Null),
        _ => {
            let (a, b) = (eligible[0].2, eligible[1].2);
            let ba = ctx.cfg.blends.iter().find(|x| x.id == a.blend).unwrap();
            let bb = ctx.cfg.blends.iter().find(|x| x.id == b.blend).unwrap();
            let t = tie_interval(ctx, ba, bb)?;
            let w = if t["includes_zero"] == true {
                if eligible[0].1 < eligible[1].1 { a } else { b }
            } else {
                a
            };
            (Some(w.name.as_str()), json!({"top_two": [a.name, b.name], "difference": "first_minus_second", "interval": t,
                "rule": "if the paired 95% interval includes zero, the earlier in the order wins (U7, R6)"}))
        }
    };
    let rec = json!({"schema": SELECTION_SCHEMA, "protocol": PROTOCOL, "rule": SELECTION_RULE, "blend_rule": BLEND_RULE,
        "order": s.order, "candidates": cands, "eligible_ranking": ranking, "tie": tie, "winner": winner,
        "outcome": match winner { Some(w) => format!("{w} is selected"), None => "no candidate is eligible: R0 stays (§9.2 step 9)".into() },
        "rank_agreement": "not computed by B10 (PENDING: needs the Dev-iter-2 evaluation and a rank statistic)",
        "margins_sha256": sha256_hex(&pretty(ctx.margins)?)});
    let bytes = pretty(&rec)?;
    Ok((
        json!({"record": s.record, "sha256": sha256_hex(&bytes), "winner": winner, "eligible_ranking": ranking}),
        bytes,
    ))
}

// ------------------------------------------------------------------ decision record (§11.10)

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExternalGate {
    pub protocol: String,
    /// A `slop_ninja_a_r2_external_gate_v1` record; its status is read from it.
    pub record: PathBuf,
    pub sha256: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DecisionSpec {
    pub candidate: String,
    pub kind: CandKind,
    pub gates: Vec<ProtocolGate>,
    #[serde(default)]
    pub external: Vec<ExternalGate>,
}

/// The levels §11.10 requires of a B10-evaluated gate (reading R16).
pub fn required_levels(protocol: &str) -> &'static [Level] {
    match protocol {
        "T3e" => &[Level::Recipe],
        "T3h" => &[Level::Artifact],
        _ => &[Level::Artifact, Level::Recipe],
    }
}

fn reported_for(kind: CandKind, protocol: &str) -> bool {
    kind == CandKind::CalibrationOnly && CALIBRATION_ONLY_REPORTED.contains(&protocol)
}

/// Structural checks of the decision record (before any read).
pub(super) fn validate_decision(cfg: &Config, d: &DecisionSpec) -> Result<()> {
    for e in &d.gates {
        ensure!(DECISION_GATES.contains(&e.protocol.as_str()), "decision: {} is not a §11.10 gate", e.protocol);
        ensure!(
            !EXTERNAL_GATES.contains(&e.protocol.as_str()),
            "decision: {} is evaluated outside B10 and enters as an external record",
            e.protocol
        );
        let g = spec_by(cfg, &e.gate, "the decision record")?;
        let want: BTreeSet<Level> = required_levels(&e.protocol).iter().copied().collect();
        let have: BTreeSet<Level> = g.levels().iter().copied().collect();
        ensure!(
            have == want,
            "decision: gate {} ({}) must be evaluated at exactly the levels §11.10 requires, {:?} (reading R16)",
            e.gate,
            e.protocol,
            want.iter().map(|l| l.name()).collect::<Vec<_>>()
        );
        if !reported_for(d.kind, &e.protocol) {
            ensure!(g.gated(), "decision: gate {} for {} is reported only and cannot be required", e.gate, e.protocol);
        }
    }
    for x in &d.external {
        ensure!(
            EXTERNAL_GATES.contains(&x.protocol.as_str()),
            "decision: {} is not one of the external gates {EXTERNAL_GATES:?}",
            x.protocol
        );
    }
    Ok(())
}

pub(super) fn decision_record(d: &DecisionSpec, gates: &[Value]) -> Result<Value> {
    let mut by: BTreeMap<&str, Vec<Value>> = BTreeMap::new();
    for e in &d.gates {
        let g = gate_by(gates, &e.gate)?;
        let levels: Map<String, Value> = g["levels"]
            .as_object()
            .map(|m| m.iter().map(|(k, v)| (k.clone(), v.get("pass").cloned().unwrap_or(Value::Null))).collect())
            .unwrap_or_default();
        for l in required_levels(&e.protocol) {
            ensure!(levels.contains_key(l.name()), "decision: gate {} has no {} level", e.gate, l.name());
        }
        by.entry(e.protocol.as_str()).or_default().push(json!({"source": "b10", "gate": e.gate,
            "block": e.block.map(Block::name), "status": g["status"], "levels": levels}));
    }
    for x in &d.external {
        let b = read(&x.record)?;
        let h = sha256_hex(&b);
        ensure!(h == x.sha256, "decision: external record {} is not {}", x.record.display(), x.sha256);
        let v: Value = serde_json::from_slice(&b).with_context(|| format!("decision: external record {}", x.record.display()))?;
        ensure!(v["schema"] == EXTERNAL_SCHEMA, "decision: external record {} is not {EXTERNAL_SCHEMA}", x.record.display());
        ensure!(
            v["protocol"] == x.protocol.as_str() && v["candidate"] == d.candidate.as_str(),
            "decision: external record {} is not {}'s {}",
            x.record.display(),
            d.candidate,
            x.protocol
        );
        let st = v["status"].as_str().unwrap_or("");
        ensure!(
            ["pass", "fail", "not_evaluated"].contains(&st),
            "decision: external record {} has status {st:?}, not pass, fail or not_evaluated",
            x.record.display()
        );
        by.entry(x.protocol.as_str()).or_default().push(json!({"source": "external", "record": x.record, "sha256": h,
            "status": st, "levels": v.get("levels").cloned().unwrap_or(Value::Null)}));
    }
    let mut rows = vec![];
    let (mut missing, mut failed, mut not_evaluated) = (vec![], vec![], vec![]);
    for p in DECISION_GATES {
        let reported = reported_for(d.kind, p);
        let entries = by.get(p).cloned().unwrap_or_default();
        let st = |s: &str| entries.iter().any(|e| e["status"] == s);
        let status = if entries.is_empty() {
            "missing"
        } else if st("fail") {
            "fail"
        } else if st("not_evaluated") {
            "not_evaluated"
        } else if entries.iter().all(|e| e["status"] == "pass") {
            "pass"
        } else {
            ensure!(reported, "decision: a required gate for {p} is reported only");
            "reported"
        };
        if !reported {
            match status {
                "missing" => missing.push(p),
                "fail" => failed.push(p),
                "not_evaluated" => not_evaluated.push(p),
                _ => {}
            }
        }
        rows.push(json!({"protocol": p, "role": if reported { "reported" } else { "required" }, "status": status,
            "required_levels": if EXTERNAL_GATES.contains(&p) { json!("external record") } else { json!(required_levels(p).iter().map(|l| l.name()).collect::<Vec<_>>()) },
            "entries": entries}));
    }
    let outcome = if !failed.is_empty() {
        "fail"
    } else if !missing.is_empty() || !not_evaluated.is_empty() {
        "incomplete"
    } else {
        "pass"
    };
    Ok(json!({"rule": "PROTOCOL-A2 §11.10 with items 1.8 and 1.9 and reading R16: a retrained candidate passes iff every listed gate passes at recipe and artifact level where defined (T3e recipe, T3h artifact, the other B10 gates both; T5a, T6a-T6c, T7a-T7c and T7e from their external records); a gate not evaluated never passes; a calibration-only candidate (R0-recal) has T1c, T3f, T3g, T5b and T5e reported; a pass is the U17 condition",
        "candidate": d.candidate, "kind": format!("{:?}", d.kind), "gates": rows, "missing": missing,
        "not_evaluated": not_evaluated, "failed": failed, "outcome": outcome}))
}

// ------------------------------------------------------------------ report sets

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReportSet {
    pub id: String,
    pub label: String,
    pub gates: Vec<String>,
}

/// Structural checks of the report sets (before any read).
pub(super) fn validate_report_sets(cfg: &Config, sets: &[ReportSet]) -> Result<()> {
    let mut ids = BTreeSet::new();
    for s in sets {
        ensure!(ids.insert(s.id.as_str()), "report set {} twice", s.id);
        for g in &s.gates {
            let spec = spec_by(cfg, g, &format!("report set {}", s.id))?;
            ensure!(
                !spec.gated(),
                "report set {}: gate {g} has a pass rule; a report set holds reported comparisons only",
                s.id
            );
        }
    }
    Ok(())
}

pub(super) fn report_sets(sets: &[ReportSet], gates: &[Value]) -> Result<Value> {
    let mut out = Map::new();
    for s in sets {
        let mut v = vec![];
        for g in &s.gates {
            let r = gate_by(gates, g)?;
            v.push(json!({"gate": g, "status": r["status"], "levels": r["levels"]}));
        }
        out.insert(s.id.clone(), json!({"label": s.label, "gates": v}));
    }
    Ok(Value::Object(out))
}

// ------------------------------------------------------------------ S6-1: replacement inputs

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DCheck {
    pub batches: PathBuf,
    pub batches_sha256: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReplacementSpec {
    /// The consumed Test-2 mock population (split test2_mock).
    pub population: String,
    /// R_0′'s testnet export on that mock, seeds 17, 29 and 41: the system
    /// behind σ′ and the d check (item 1.13).
    pub system: String,
    #[serde(default)]
    pub sigma: Option<PathBuf>,
    #[serde(default)]
    pub sigma_sha256: Option<String>,
    #[serde(default)]
    pub d_check: Option<DCheck>,
    /// The pure-human FPR at 0.5: (population, system) pairs.
    #[serde(default)]
    pub human_fpr: Vec<BlendTerm>,
}

/// Structural checks of the replacement inputs (before any read).
pub(super) fn validate_replacement(cfg: &Config, r: &ReplacementSpec) -> Result<()> {
    let p = cfg
        .populations
        .iter()
        .find(|p| p.id == r.population)
        .with_context(|| format!("replacement: unknown population {}", r.population))?;
    ensure!(
        p.split == "test2_mock" && p.block == Block::Mock,
        "replacement: population {} is not the consumed Test-2 mock (split test2_mock)",
        p.id
    );
    ensure!(
        matches!(p.weights, WeightSpec::Field { .. }),
        "replacement: the consumed mock's weights are not a W_A8 row field"
    );
    let seeds: BTreeSet<u64> = cfg
        .systems
        .iter()
        .filter(|s| s.population == r.population && s.name == r.system)
        .map(|s| s.seed)
        .collect();
    ensure!(
        seeds == RECIPE_SEEDS.iter().copied().collect(),
        "replacement: {} needs seeds {RECIPE_SEEDS:?} on {} (item 1.13), not {seeds:?}",
        r.system,
        r.population
    );
    ensure!(
        r.sigma.is_some() == r.sigma_sha256.is_some(),
        "replacement: sigma and sigma_sha256 come together"
    );
    for t in &r.human_fpr {
        ensure!(
            cfg.systems.iter().any(|s| s.population == t.population && s.name == t.system),
            "replacement: {} has no predictions on {}",
            t.system,
            t.population
        );
    }
    Ok(())
}

/// `RSS(x | y)` of the reward-v1 manifest (subnet protocol crate
/// `reward_v1::aligned_rss`, on parts-per-billion integers; pinned by a test
/// against that function's recorded outputs).
pub fn aligned_rss(x: &[u32], y: &[u32]) -> Result<u128> {
    ensure!(x.len() == y.len(), "prediction vectors");
    let mut order: Vec<usize> = (0..x.len()).collect();
    order.sort_unstable_by_key(|&i| y[i]);
    let mut blocks: Vec<(u128, u128)> = Vec::new();
    let mut squares = 0u128;
    let mut k = 0;
    while k < order.len() {
        let level = y[order[k]];
        let (mut sum, mut count) = (0u128, 0u128);
        while k < order.len() && y[order[k]] == level {
            let v = u128::from(x[order[k]]);
            sum += v;
            squares = squares.checked_add(v * v).context("aligned squares")?;
            count += 1;
            k += 1;
        }
        blocks.push((sum, count));
        while let [.., (s1, c1), (s2, c2)] = blocks[..] {
            let lower = s1.checked_mul(c2).context("aligned pooling")?;
            let upper = s2.checked_mul(c1).context("aligned pooling")?;
            if lower <= upper {
                break;
            }
            blocks.pop();
            *blocks.last_mut().expect("two blocks") = (s1 + s2, c1 + c2);
        }
    }
    let mut fitted = 0u128;
    for (sum, count) in blocks {
        fitted += sum.checked_mul(sum).context("aligned fit")? / count;
    }
    squares.checked_sub(fitted).context("aligned residual")
}

/// Near-copy statistics over the documents both returned: (common, min RSS).
pub fn near_min_rss(x: &[Option<u32>], y: &[Option<u32>]) -> Result<(u32, u128)> {
    let (xs, ys): (Vec<u32>, Vec<u32>) = x.iter().zip(y).filter_map(|(a, b)| a.zip(*b)).unzip();
    Ok((xs.len() as u32, aligned_rss(&xs, &ys)?.min(aligned_rss(&ys, &xs)?)))
}

/// `A_sym ≤ d` without a square root: common > 0 and min RSS ≤ common · d².
pub fn near_within(common: u32, min_rss: u128, d_ppb: u32) -> bool {
    common > 0 && min_rss <= u128::from(common) * u128::from(d_ppb) * u128::from(d_ppb)
}

/// Involvement in parts per billion, as the manifest's `Probability::from_f64`
/// (reading R12): a non-finite or out-of-range value is refused, never
/// clamped; `p · 10^9` is rounded half to even.
pub fn ppb(p: f64) -> Result<u32> {
    ensure!(p.is_finite() && (0.0..=1.0).contains(&p), "involvement {p} is not a probability in [0, 1]");
    Ok((p * 1e9).round_ties_even() as u32)
}

/// σ′ and σ_eff from B12's record (items 1.13, 1.2, 12.3 and 12.4).
fn replacement_sigma(ctx: &Ctx, p: &Population, r: &ReplacementSpec, path: &Path, h: &str) -> Result<Value> {
    let field = match &ctx.cfg.populations.iter().find(|x| x.id == p.id).context("replacement population")?.weights {
        WeightSpec::Field { field } => field.clone(),
        _ => bail!("replacement: the consumed mock's weights are not a W_A8 row field"),
    };
    let (v, d) = sigma_design(path, h, SIGMA_N_PRIME, &field, "replacement")?;
    ensure!(
        v["inputs"]["rows_sha256"] == p.rows_sha256.as_str(),
        "replacement: the sigma record is not on the consumed Test-2 mock's rows"
    );
    // The record's three systems are R_0′'s seeds 17, 29 and 41, in order, by their files' digests.
    let seeds = runs(ctx.book, &p.id, &r.system)?;
    let names: Vec<&str> = v["systems"].as_array().into_iter().flatten().filter_map(Value::as_str).collect();
    ensure!(names.len() == 3, "replacement: the sigma record does not name three systems");
    for (k, (name, seed)) in names.iter().zip(RECIPE_SEEDS).enumerate() {
        let got = &v["inputs"]["predictions"][*name]["sha256"];
        ensure!(
            *got == seeds[&seed].pred_sha256.as_str(),
            "replacement: the sigma record's system {} ({name}) is not {}'s seed {seed} predictions",
            k + 1,
            r.system
        );
    }
    let scaled = |x: &Value, what: &str| -> Result<u128> {
        x.as_str().and_then(|s| s.parse::<u128>().ok()).with_context(|| format!("replacement: the sigma record's {what}"))
    };
    let sigma_scaled = scaled(&d["sigma_scaled"], "sigma_scaled")?;
    let eff = &d["sigma_eff"];
    ensure!(
        eff.is_object() && scaled(&eff["sigma_m_scaled"], "sigma_m_scaled")? == u128::from(SIGMA_M_SCALED),
        "replacement: the sigma record has no sigma_eff with the manifest's sigma_m ({SIGMA_M_SCALED} on the 1e-18 grid; item 1.2)"
    );
    let eff_scaled = scaled(&eff["sigma_eff_scaled"], "sigma_eff_scaled")?;
    let delta_scaled = scaled(&eff["delta_eff_scaled"], "delta_eff_scaled")?;
    ensure!(
        eff_scaled == sigma_scaled.max(u128::from(SIGMA_M_SCALED)) && delta_scaled == DELTA_FLOOR_SCALED.max(2 * eff_scaled),
        "replacement: the sigma record's sigma_eff or delta does not follow item 1.2 (sigma_eff = max(sigma_m, sigma'), delta = max(1%, 2 sigma_eff))"
    );
    let as_f64 = |x: u128| x as f64 / SCORE_GRID as f64;
    Ok(json!({"record": path, "sha256": h, "n": SIGMA_N_PRIME, "mix": SIGMA_MIX, "weight_field": field,
        "systems": names, "seeds": RECIPE_SEEDS,
        "sigma_prime": d["sigma"], "sigma_scaled": sigma_scaled.to_string(),
        "sigma_m_scaled": SIGMA_M_SCALED.to_string(), "sigma_eff_scaled": eff_scaled.to_string(), "sigma_eff": as_f64(eff_scaled),
        "delta_scaled": delta_scaled.to_string(), "delta": as_f64(delta_scaled),
        "rule": "PROTOCOL-A2 item 1.13 with items 1.2, 12.3 and 12.4: sigma' from B12 (the pinned stratified bootstrap SE of q at n = 1,000 under 20/55/25 with the mock's W_A8 field, pooled over R_0′'s seed pairs 17-29, 17-41 and 29-41, 1e-18 grid); sigma_eff = max(sigma_m, sigma'), delta = max(1%, 2 sigma_eff) (item 1.2: the bar never falls); w = 2 sigma_eff (1 - S_Rk) and epsilon = w/250 are per cohort"}))
}

pub(super) fn replacement(ctx: &Ctx, r: &ReplacementSpec) -> Result<Value> {
    let p = pop(ctx, &r.population, "replacement")?;
    ensure!(p.split == "test2_mock", "replacement: population {} is not the consumed Test-2 mock (split test2_mock)", p.id);
    let seeds = runs(ctx.book, &p.id, &r.system)?;
    ensure!(seed_list(seeds) == RECIPE_SEEDS, "replacement: {} needs seeds {RECIPE_SEEDS:?}", r.system);
    let mut out = Map::new();
    out.insert("population".into(), json!({"id": p.id, "rows_jsonl_sha256": p.rows_sha256}));
    out.insert("system".into(), json!(r.system));
    if let (Some(path), Some(h)) = (&r.sigma, &r.sigma_sha256) {
        out.insert("sigma".into(), replacement_sigma(ctx, p, r, path, h)?);
    }
    if let Some(dc) = &r.d_check {
        let b = read(&dc.batches)?;
        ensure!(sha256_hex(&b) == dc.batches_sha256, "replacement: the batches file is not {}", dc.batches_sha256);
        let at: BTreeMap<&str, usize> = (0..p.primary.len()).map(|q| (p.rows[p.primary[q]].id.as_str(), q)).collect();
        let mut batches = vec![];
        for (k, l) in std::str::from_utf8(&b)?.lines().enumerate() {
            if l.trim().is_empty() {
                continue;
            }
            let v: Value = serde_json::from_str(l).with_context(|| format!("batches line {}", k + 1))?;
            let ids = v["ids"].as_array().with_context(|| format!("batches line {}: ids", k + 1))?;
            ensure!(ids.len() == D_CHECK_BATCH_DOCS, "batches line {}: {} documents, not {D_CHECK_BATCH_DOCS}", k + 1, ids.len());
            let pos = ids
                .iter()
                .map(|x| x.as_str().and_then(|s| at.get(s).copied()).with_context(|| format!("batches line {}: an id outside the population", k + 1)))
                .collect::<Result<Vec<usize>>>()?;
            ensure!(
                pos.iter().collect::<BTreeSet<_>>().len() == pos.len(),
                "batches line {}: a document twice (a batch is 1,024 distinct documents)",
                k + 1
            );
            batches.push(pos);
        }
        ensure!(batches.len() == D_CHECK_BATCHES, "replacement: {} batches, not {D_CHECK_BATCHES}", batches.len());
        let vec_of = |s: u64| -> Result<Vec<Option<u32>>> {
            let run = &seeds[&s];
            (0..p.primary.len())
                .map(|q| match run.predictions.get(&p.rows[p.primary[q]].id) {
                    Some(Obs::Ok(_)) => ppb(run.inv[q]).map(Some),
                    _ => Ok(None),
                })
                .collect()
        };
        let mut pairs = vec![];
        let mut any = false;
        for (a, bb) in SEED_PAIRS {
            let (va, vb) = (vec_of(a)?, vec_of(bb)?);
            let (mut within, mut min_sq): (usize, Option<u128>) = (0, None);
            for bt in &batches {
                let x: Vec<Option<u32>> = bt.iter().map(|&q| va[q]).collect();
                let y: Vec<Option<u32>> = bt.iter().map(|&q| vb[q]).collect();
                let (c, m) = near_min_rss(&x, &y)?;
                if near_within(c, m, D_CHECK_D_PPB) {
                    within += 1;
                }
                if c > 0 {
                    let sq = m / u128::from(c);
                    min_sq = Some(min_sq.map_or(sq, |x| x.min(sq)));
                }
            }
            any |= within > 0;
            pairs.push(json!({"seeds": [a, bb], "batches_within": within, "min_distance_ppb": min_sq.map(|x| x.isqrt() as u64)}));
        }
        out.insert("d_check".into(), json!({"system": r.system, "batches": dc.batches, "batches_sha256": dc.batches_sha256,
            "d_ppb": D_CHECK_D_PPB, "pairs": pairs, "d_stays": !any,
            "rule": "PROTOCOL-A2 item 1.13 with reading R12: A_sym <= d iff min(RSS(x|y), RSS(y|x)) <= common x d^2 on the documents both seeds returned; d stays only if no seed pair falls within d on any batch",
            "outcome": if any { "a seed pair falls within 0.025: d is not re-derived automatically; the replacement waits for an amendment (item 1.13)" } else { "no round-2 seed pair falls within A_sym 0.025: d stays at 0.025 (item 1.13)" }}));
    }
    let mut fpr = vec![];
    for t in &r.human_fpr {
        let q = pop(ctx, &t.population, "replacement")?;
        let seeds = runs(ctx.book, &t.population, &t.system)?;
        let mut per = Map::new();
        for (s, run) in seeds {
            let k = q.humans.iter().filter(|&&h| run.flag[h] >= FLOOR_THRESHOLD).count();
            per.insert(s.to_string(), json!({"flags": k, "humans": q.humans.len(), "rate": (!q.humans.is_empty()).then(|| k as f64 / q.humans.len() as f64)}));
        }
        fpr.push(json!({"population": t.population, "system": t.system, "seeds": per}));
    }
    out.insert("human_fpr_at_0_5".into(), json!({"threshold": FLOOR_THRESHOLD, "entries": fpr,
        "role": "reported: the strict pure-human tier's counts on the mock's pure-human negatives and Test-2 T humans (item 1.13); the manifest's floor values and comparison rule are applied when they are fixed"}));
    Ok(Value::Object(out))
}
