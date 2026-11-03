# Competitive A evaluator

`grammar/crates/grammar-a-baseline/src/comp_eval.rs` computes the metrics of
[a-competitive-protocol.md](a-competitive-protocol.md) from a bundle and one
predictions file per system and seed. The CLI is the example
`a_comp_evaluate`. It is Rust, makes no network or model calls, and never
reads a text field: only metadata and predictions.

```
a_comp_evaluate --bundle DIR --split dev_iter \
  --pred E1:17=e1-s17.jsonl --pred E1:29=e1-s29.jsonl --pred E1:41=e1-s41.jsonl \
  --pred E0:17=e0-s17.jsonl ... \
  [--ops E1:17=ops-s17.json ...] [--subsample pangram=ids.txt] \
  [--compare E1,E0 ...] [--held-out-generator LABEL ...] \
  [--frontier-label LABEL ...] [--released-seed 17] \
  [--probes probes.jsonl --probe-pred E1:17=probe-s17.jsonl ...] \
  [--train-manifest TRAIN/MANIFEST.json] [--ood-domain DOMAIN ...] \
  [--consume-sealed-split SPLIT] --out OUT
```

It writes `OUT/evaluation.json` (schema `slop_ninja_a_comp_evaluation_v1`)
and `OUT/evaluation.md`, and records the SHA-256 of every input.

## Sealed splits

`dev_select`, `cal` and `test` are refused unless `--consume-sealed-split`
names the same split, and for them an existing `evaluation.json` is never
overwritten. Dev-select is opened once; CAL and Test only at their protocol
steps. Every other split (Dev-iter, synthetic fixtures) needs no flag.

`test` is also refused unless every seed of every system has `--ops` with
both CAL-declared thresholds (0.01 and 0.001). Tier C condition 2 uses the
CAL operating point, so an in-split threshold is never computed on Test.

## Bundle input

The bundle is an assembler split directory (`slop_ninja_a_assemble build`
output). The evaluator reads `rows.jsonl` and hashes `weights.json` and
`MANIFEST.json`. If the manifest records a digest under the key `rows.jsonl`
(a hex string or an object with `sha256`), it must match. Unknown fields,
including `text` and `text_ref`, are ignored and never examined.

| Field | Type | Rule |
|---|---|---|
| `id` | string | unique |
| `split` | string | rows of other splits are ignored |
| `origin` (alias `origin_class`) | `human_only`, `mixed`, `model_only` | |
| `share` (alias `measured_share`) | number | realised final-word fraction: 0 for humans, 1 for model-only, (0, 1] for mixed |
| `spans` (aliases `replacement_spans`, `ai_byte_spans`) | `[[start, end], ...]` byte offsets | sorted, non-overlapping, empty for humans; not used by any metric |
| `generator_label` (aliases `attribution_label`, `generator_attribution_label`) | string or null | canonical attribution label; null exactly for humans |
| `generator_family` | string, optional | otherwise derived (below) |
| `cohort_id` (alias `cohort`) | string | reported as the `cohort` slice; it does not decide the population |
| `cohort_weight` | number | training-side only; not used by any metric |
| `source_group` (aliases `group`, `source_family`) | string | the bootstrap and balancing unit |
| `domain` | string | |
| `genre`, `voice_id`, `voice_cluster` (alias `voice_cluster_id`), `voice_origin` | string or null | |
| `style_axes` | null, string, array of axis names, or object | an object contributes the values of its `axis`, `library_axis` and `persona_axis` keys; its other keys are provenance |
| `operation` | `mirror`, `span`, `full`, `continuation`, `splice`, or another lowercase label (`polish`, `condense`, ...) in a supplementary population; null, `none` or `human` for humans | |
| `held_out_style` | bool | default false |
| `population` | string, required | the assembler's population label (below) |
| `voice_kind` | string or null | null for a plain positive; any value (`voice_prompt`, `brief`) is voiced |
| `ocr_tier` | string or number, optional | OCR tier of the row (positives inherit their parent's) |
| `markup_removed` or `removed_markup_per_100_words` | bool or number, optional | removed-markup presence (a positive rate means present) |
| `demographics` | object of scalars, optional | released learner demographics (for example English-learner status, grade) |
| `human_attribution_label` | string or null, optional | the named human profile of an enrolled writer; null when none is enrolled |

**Primary population.** The `population` field decides it; nothing is
inferred from cohorts or operations. Rows with population `primary` or
`frontier` are primary, as protocol v1.2 states: Test humans, their instruct
positives and the frontier Test positives, which share their cell's mass in
the balanced weights and their domain's Pangram strata. Every other
population (`robustness*`, the ungated human-only diagnostics such as
`sbcsae_spoken_conversation` and `ellipse_ell_learner_essays`, or any other
label) is outside the primary metric, the FPR gate and the Pangram strata,
and is reported as its own `population` slice. The rows are checked for
consistency: primary positives must be mirrors, span rewrites or full
rewrites, robustness positives continuations or splices, positives of any
other population may carry the generator's own lowercase operation label,
and the frontier population holds no humans.

The trainer projection (`slop_ninja_a_assemble trainer-bundle`) has no
`population` field, so predictions are evaluated against the assembler split
directory, whose row ids are the same.

**Generator family.** When a row names none, the family is the attribution
label with any `requested:` prefix dropped. Hosted labels beginning `claude-`
or `gpt-` map to the vendor lineages `claude` and `gpt`. Any other label is
its own family. A bundle whose base and instruct checkpoints share a
pretraining lineage (for example `smollm3_base` and `smollm3_instruct`) sets
`generator_family` explicitly, because the protocol defines a family as a
lineage.

## Predictions input

One JSON object per line:

| Field | Rule |
|---|---|
| `id` | a document of the evaluated split; no duplicates |
| `p_human`, `p_model_only`, `p_mixed` | in [0, 1], summing to 1 within 1e-6 |
| `share` | in [0, 1] |
| `attribution` (aliases `attribution_probabilities`, `ai_attribution`) | optional map label to probability (the conditional AI scores), summing to 1 within 1e-6; the same label set on every row of a file. The shared role form `{"human": {...}, "ai": {...}}` is also accepted here |
| `role_attribution` | optional role form: `{"human": R, "ai": R}` with `R = {"profile", "involvement_probability", "conditional_named_scores"}`. `profile` is the completed result, a roster profile or `other_human` / `other_ai`; the named scores are the role's roster and sum to 1 when present. Without a flat `attribution`, the AI scores are the `ai` role's named scores |
| `status`, `failure` | optional; any status other than `ok` is an execution failure |

A null output is a non-finite output, so an execution failure. A finite value
that breaks the contract (out of range, wrong sum) is an input error and
stops the evaluation.

**Operating points** (`--ops`) are the CAL-declared thresholds for one seed:
`{"0.01": t, "0.001": t}` (optionally under `thresholds`). Each `t` is a
number, `"inf"`, or `{"bits": "0x…"}` with an optional `value` that must
agree. Without `--ops` the evaluator applies the protocol rule to the
evaluated split itself: on CAL that is the declared operating point (labelled
`in_split_cal_rule`); on Dev-iter and Dev-select it is labelled
`in_split_diagnostic`; on Test it is refused.

## Failures and missing observations

- **Execution failure**, including a document with no prediction: involvement
  is `1 − y`, so a human is flagged and a positive missed. The three-way
  output puts all mass on a wrong class (model-only for a human, human for a
  positive). Share takes the far end (0 when the true share is at least 0.5,
  otherwise 1). Attribution counts as wrong. Every such document stays in
  every denominator.
- **Subsample comparator** (`--subsample NAME=ids`): the system is observed
  only on the pre-registered selection. A selected document without a valid
  observation is scored at its best case (involvement equals the label, the
  true origin and share). A prediction outside the selection is an error.
  `subsample_missing` reports the largest missing count over seeds and
  whether it exceeds 2% of the selection, the protocol's Tier C limit.

## Weights

- **Balanced:** equal source-group mass, equal mass per observed origin cell
  within a group, equal mass per document within a cell
  (`pangram_subsample::balanced_weights`).
- **Family-weighted:** equal group mass, equal document mass within a group,
  so the observed origin mix.
- A slice recomputes the weights over its own documents and groups.
- Training cohort weights in `weights.json` are not evaluation weights.
- **Subsample systems.** Every weighted metric of a subsample system (the
  balanced and family Brier and their interval, human mean Brier, ECE10,
  share MAE, MSE and bias, three-way Brier, log loss and weighted accuracy,
  role involvement Brier) is the Horvitz-Thompson estimate of the full-split
  value: the weight of an observed document is its balanced (or family)
  weight over the full primary population of the metric's block, divided by
  `π_h = n_h / N_h`, and an unobserved document contributes 0. The interval
  resamples the group values of every group of the population. Counts and
  rates (FPR, TPR, unweighted accuracy, attribution) stay over the observed
  documents, so Pangram's FPR and TPR are on its subsample humans, as the
  protocol asks. Slices outside the primary population are not estimated for
  a subsample system.

## Metrics, per system and seed

All metrics are computed on the primary population unless a slice is named.
Involvement is `p_model_only + p_mixed`, and the label is 1 for any model
involvement.

| Metric | Definition |
|---|---|
| Balanced involvement Brier | Σ w_d (inv_d − y_d)² with balanced weights; interval from the protocol bootstrap over group values `v_g = G Σ_{d∈g} w_d x_d` |
| Family-weighted Brier | the same loss with family weights |
| Human FPR at 0.5 | humans with involvement ≥ 0.5, counted once each and unweighted; one-sided 95% and two-sided 95% Clopper-Pearson bounds; source-group ratio bootstrap (same generator and draw indices); the Tier C flag limit (largest count whose one-sided bound is at most 2.5%) for the realised human count |
| Operating point | protocol rule: the smallest value `v` among the distinct involvement values of the split's documents (any origin) plus +∞ such that at most α of the humans have a value ≥ `v`; α is exact (1/100, 1/1000); ties count as flagged; the threshold is reported with its f64 bits |
| Human FPR and TPR at 1% and 0.1% | flags ≥ t among humans, and among primary positives, with the same bounds |
| ECE10 | ten equal-width bins `min(floor(10 p), 9)`, Σ_b \|Σ w p − Σ w y\| with balanced weights; overall and on humans (balanced over the humans' groups) |
| Share MAE, MSE and bias | in percentage points, balanced within each block: overall, mixed, and each realised-share bin (`human_only`, `model_only`, `mixed:(0,10)`, `[10,25)`, `[25,50)`, `[50,75)`, `[75,90)`, `[90,100)`, `100`) |
| Share consistency | count of predictions whose share lies outside `[p_model_only, involvement]` |
| Three-way | class order human_only, model_only, mixed; balanced sum-of-three Brier, balanced log loss (p clipped at 1e-15), balanced weighted accuracy, unweighted accuracy, unweighted macro-F1, confusion matrix; argmax ties go to the earlier class |
| Attribution | over positives with a label. The target is the label when the model's vocabulary contains it, otherwise `other_ai`; the remapped count is reported. Prediction is the argmax (ties to the smallest label). Accuracy, label-balanced accuracy, unweighted macro-F1 over the union of target and predicted labels (precision 0 for a never-predicted label, F1 0 when undefined), per-target-label precision, recall and F1, and accuracy per original generator label |

Attribution is reported four ways: primary positives, primary positives with
held-out voices, all positives of the split (including robustness and other
non-primary populations), and all positives with held-out voices.

**Frontier attribution** (`frontier_attribution`) follows the protocol:
top-1 model attribution among the frontier labels and `other_ai`. The
candidates are the frontier labels of the model's vocabulary plus `other_ai`,
whose score is every other label's conditional mass (local families and any
explicit `other_ai`). The target is the row's label when it is a frontier
label of the vocabulary, otherwise `other_ai`. It is scored on the frontier
positives, on held-out voices (`held_out_style`) and on all voices, with
accuracy, macro-F1 and accuracy per frontier label. The frontier roster is
`--frontier-label` (the seal's pinned roster), or by default the labels of
the split's frontier rows; a frontier row outside a declared roster is an
error. The unrestricted argmax figures stay separate.

**Role attribution** (`role_attribution`) scores the completed human and AI
results on every document that documents a contributor of the role,
regardless of predicted involvement: humans and mixed documents for the
human role, model-only and mixed documents for the AI role. Documents
without that contributor are not scored for the role (a conditional result
for an absent role asserts nothing). The target is the documented
contributor's profile (`human_attribution_label`, or the generator label)
when the role's roster holds it, otherwise `other_human` or `other_ai`; the
prediction is the returned profile. Each role also reports a balanced Brier
of its involvement probability against contributor presence. A system
without completed AI results is scored by the argmax of its conditional
scores; a system without a human role output is reported as unavailable.

## Recipe and released artifact

- **Recipe:** the balanced Brier and its interval use per-document losses
  averaged over the seeds before groups are resampled. By linearity the point
  value equals the seed mean. Other metrics are seed means. Flag counts
  report the maximum over seeds, as the FPR gate's headline figure.
- **Released artifact:** the `--released-seed` fit (default 17). A system with
  one fit, such as a frozen comparator, uses that fit.

## Slices

Each slice is reported per seed, with point values for every value and
intervals for every dimension except `voice_cluster`,
`held_out_voice_cluster` and `human_style_axis`, which can have thousands of
values. Slice records are compact: counts, the balanced and family-weighted
Brier, human and positive mean Brier, mean human involvement, FPR and TPR at
0.5 and at the operating points with the one-sided bound, ECE10, attribution
accuracy and macro-F1, and for the `frontier*` dimensions the frontier
attribution. The FPR source-group bootstrap and the calibration bins are
reported for the primary population only.

| Kind | Dimensions | Documents |
|---|---|---|
| `rows` | `domain`, `genre`, `ocr_tier`, `removed_markup` (`present`, `absent`) | the matching primary rows (humans and positives) |
| `positives_with_all_humans` | `generator`, `generator_family`, `frontier` (`all`: the pooled frontier slice), `frontier_model`, `frontier_voicing` (`plain`, `voiced`), `cohort`, `operation`, `voice_cluster`, `voice_origin`, `style_axis`, `held_out_style`, `held_out_style_axis`, `held_out_voice_cluster`, `generator_held_out_style`, `held_out_generator` (from `--held-out-generator`), `realised_share` | the matching primary positives plus every primary human, balanced like the primary metric |
| `humans` | `human_genre` (`unspecified` when null), `human_style_axis`, `demographic:<field>` | the matching humans: human Brier, FPR, mean involvement and ECE. Demographic slices include humans of diagnostic populations |
| `population_rows` / `positives_with_all_humans` | `population` | each non-primary population: its own rows when it has humans (human-only diagnostics: FPR at the operating points and mean involvement), otherwise its positives plus every primary human (robustness) |

**Realised minimum.** The protocol's minimum is decided from pipeline
counts, independent of any system: a slice below 15 source groups or, when it
has humans, below 150 humans is `below_slice_minimum`; the pooled `frontier`
slice also needs 150 frontier positives. The top-level `gated_slices` lists
the protocol's fixed gated slices (the fifteen gated domains,
`held_out_generator:all` and `frontier:all`) with their realised counts, any
shortfall, and status `gated` or `reported_ungated`. A listed slice with no
documents is `reported_ungated`. Each comparison variant repeats that list
with the slice's difference and interval, which is the Tier C condition 3
input.

## Paired comparisons

`--compare A,B` reports A minus B for the recipe and for the released
artifact, on the primary population and on every slice:

- **Full mode** (neither side a subsample). Per-document differences of the
  involvement loss, seed-averaged on each side for the recipe. Then:
  - the balanced point value is the mean of the group values;
  - the interval comes from the protocol bootstrap (1,000 draws, xorshift seed
    20261026, `draws[24]` and `draws[974]`);
  - family-weighted Brier and share absolute error get the same treatment.
- **Horvitz-Thompson mode** (one side observed on its subsample). Inclusion
  probabilities are `n_h / N_h` over the split's primary strata
  (`domain|human_only`, `domain|model_only`, `domain|mixed:<bin>`), frontier
  positives included. Each slice uses the slice's own balanced weights and
  groups with every document's `π_h`, and an unselected document contributes
  zero. The estimate is the mean of `v_g = G Σ_{d∈g, selected} w_d Δ_d / π_h`.
  The family-weighted Brier difference and the share absolute-error
  difference (A's share MAE against Pangram's raw-fraction MAE) use the same
  estimator. Slices outside the primary population are left out.

The intervals exclude seed, training-data and calibration-estimation
uncertainty, as the protocol states.

## Ungated diagnostics

`comp_diagnostics.rs` adds two diagnostics under `diagnostics` in
`evaluation.json`, plus a section of `evaluation.md` for each. Neither enters
the primary metric, a gate or the Pangram strata. Both read metadata and
predictions only.

### Answer consistency

`--probes` names a probe manifest (`slop_ninja_a_consistency_probe_v1`)
written by `slop_ninja_a_augment probes` from the split's bundle rows:

- `repeat` probes are exact repeats; their digests must match and they record
  no edits;
- `perturbed` probes are light noised copies (profile `light_v1`, at least
  one edit).

A system is run on the manifest's probe rows. `--probe-pred NAME:SEED=PATH`
then supplies those predictions, which use the ordinary prediction format;
the same system and seed must also have a `--pred`. Input errors are:
- a probe id that is also a document id;
- a base outside the split;
- a repeat whose digests differ or that records edits;
- a prediction for an unlisted probe.

For each system and seed, and for each kind (overall, per origin and, for
perturbations, per noise operation and profile), the report gives:
- the signed change in P(involvement) (probe minus base), with a 95%
  source-group bootstrap interval of its mean;
- the mean, maximum and p50/p90/p99 absolute change, and the counts at or
  above 0.01, 0.05 and 0.10;
- bit-identical and two-decimal-equal counts;
- the mean absolute share change;
- decision flips at 0.5 and at the seed's operating points (threshold bits
  read from the evaluation), split into newly flagged and newly unflagged.

`repeats_all_bit_identical` states whether serving is deterministic. A
missing or failed base or probe observation is counted in `accounting` and
not scored.

### Out-of-domain human FPR

`--train-manifest` names the Train split `MANIFEST.json` of the same build.
From its `counts_domain_origin_generator`, a human of the evaluated split, in
any population including the diagnostic ones, is out of domain when:
- its domain has no Train rows (`domain_absent_from_train`);
- its domain has Train positives but no Train humans
  (`domain_without_train_humans`);
- or its domain is named by `--ood-domain` (`declared`).

Without a manifest, only declared domains count.

The report gives human FPR at 0.5 and at the operating points, with one-sided
95% Clopper-Pearson upper bounds, and mean involvement. It covers
out-of-domain humans, in-domain humans of the same split for comparison, and
the out-of-domain humans by reason, domain and population. A missing or
failed observation counts as flagged, the execution-failure rule.

Attribution: re-querying a detector on repeated text and keeping an
out-of-domain human check follow ideas in the MIT-licensed It's AI validator
(<https://github.com/It-s-AI/llm-detection>, Copyright 2024 It's AI). Here
both are reported diagnostics with intervals, not penalties. The probe
format, statistics and domain rule are our own.

## Tests

`cargo test -p grammar-a-baseline --lib comp_` runs the protocol metric tests
and the diagnostics tests. The diagnostics tests cover:
- probe manifest validation;
- consistency deltas, quantile counts, flips at 0.5 and at an operating
  point;
- the accounting of missing and failed observations;
- the out-of-domain rule and FPR, including the failure rule;
- exact threshold bits read from a real evaluation.

The `comp_eval` tests check, on synthetic data with hand-computed values:
- balanced and family-weighted Brier;
- ECE10, share MAE, MSE and bins, and three-way Brier, accuracy, macro-F1 and
  confusion;
- the operating-point rule, including ties and +∞, and declared thresholds;
- worst-case failures;
- seed averaging, and an exact constant-shift difference with a degenerate
  interval;
- the HT identity at full sampling, and HT unbiasedness over every
  one-per-stratum sample with a best-case missing observation;
- equality of the generic bootstrap with `bootstrap_interval`;
- Clopper-Pearson lower and upper symmetry;
- attribution accuracy, macro-F1 and the `other_ai` remap;
- slice membership by population, the sealed-split guard and input
  validation;
- frontier rows in the primary metric, the pooled frontier slice and the HT
  strata;
- unbiasedness of every weighted standalone metric of a subsample system over
  every one-per-stratum sample, and equality with the full values at full
  sampling;
- the HT share-MAE and family-Brier differences at full sampling, and an
  exact share-MAE difference;
- the Test refusal without declared operating points;
- frontier top-1 attribution among frontier labels and `other_ai`, on
  held-out and all voices, beside the unrestricted figure;
- the realised minimum of the pooled frontier slice (150 against 149
  positives) and the gated list;
- diagnostic populations outside the primary metric, with their FPR at the
  operating points and mean involvement;
- human- and AI-role attribution with `other_human` and `other_ai`, and role
  involvement Brier.

## Tier C and Tier I

Three examples of `grammar-a-baseline` run the protocol's Pangram and gate
steps. None sends a request or reads a text field other than for word counts
and hashes.

- `a_comp_pangram_cal_map` fits the Pangram CAL map from the CAL subsample
  (`a_comp_pangram_cal_subsample` output) and the `a_comp_pangram_bulk`
  stores, given in dispatch order. The pinned version is that of the first
  stored success: the first store, then the first job in `PLAN.json` order,
  then the lowest `bulk_index`. A success of another version, a failure
  (including an echo mismatch beyond whitespace), a job not sent, a job
  never submitted and an uncertain item are missing. A duplicate text skipped
  by the collector takes its twin's observation. An in-flight or unreceipted
  job stops the tool unless it is declared uncertain (`STORE_INDEX:JOB`). A
  later store may only re-send items that were uncertain or never submitted.
  More than 2% missing writes a `refused_tier_c_not_evaluated` record and
  exits 3. Otherwise the tool fits `isotonic_v1_clipped` on
  `min(1, fraction_ai + fraction_ai_assisted)` with weight
  `balanced_weight / pi_h` through A's calibrator code path
  (`involvement_calibration::fit_isotonic_scores`). The clip bounds come from
  the valid human and positive counts. It writes `map.json`, with the 1% and
  0.1% operating points as f64 bits from `operating_threshold` over the valid
  subsample documents, and `cal-observations.jsonl`.
- `a_comp_pangram_test_subsample` draws the Test subsample from the primary
  Test rows (populations `primary` and `frontier`). The strata are
  `Row::stratum`, `W_h` comes from the full primary Test balanced mass, and
  the planning SDs are checked bit for bit against `protocol-v1.json`. It uses
  floor 3, `neyman_allocation`, size `min(n_test, population)` and
  `selection_key(20261027, "test", id)`. It writes `population.jsonl`,
  `subsample.jsonl` (the collector's input) and `design.json`.
- `a_comp_tierc_evaluate` computes the gates in `tierc` on the pinned
  `pangram_subsample` estimator, for the recipe (seeds 17, 29 and 41) and the
  released artifact (seed 17):
  - Condition 1: the HT estimate and `draws[974]` of the full primary Test
    difference. A missing Pangram document scores involvement equal to the
    label. Group values are cross-checked against `ht_group_values`.
  - Condition 2: flags at each seed's CAL 1% point over every primary Test
    human, against `max_flags(n, 0.95, 0.025)`. A failed human counts as
    flagged at any threshold.
  - Condition 3: HT gap point estimates of the gated slices that meet the
    realised minimum.
  - Condition 4: execution failures of the claimant's fits.
  - Tier I against the frozen stage-3 A: the paired full-Test interval must
    lie below zero, and mean human FPR at 0.5 must be no higher.

  Tier C is `not_evaluated` when `n_test` is below n_min, the CAL map was
  refused, or more than 2% of the selected Test documents are missing. Tier I
  is evaluated regardless. Every other metric comes from `comp_eval::evaluate`
  over all systems, including CAL-mapped Pangram on its subsample:
  `p_model_only` carries the calibrated involvement and `share` the raw
  clamped fraction, so Pangram's three-way and attribution figures are not
  meaningful. The claimant is compared with every other system, and every A
  system with Pangram. The gate path and comp_eval are cross-checked.
  Pangram's FPR and TPR at its own CAL points are reported on the subsample
  documents with valid observations. The tool writes `report.json`,
  `summary.txt` and `evaluation.md` once and refuses to overwrite them.

## Limits

- The bundle does not yet carry `ocr_tier`, removed-markup rates,
  `demographics` or `human_attribution_label`; their slices and the named
  human-profile targets appear once the assembler writes them. Until then
  every human target is `other_human`.
- `a_comp_evaluate` does not compute Tier C and Tier I decisions;
  `a_comp_tierc_evaluate` does (see above).
