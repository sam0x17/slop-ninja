# Competitive A baseline protocol

This protocol pre-registers the claim, populations, Test construction,
selection rule and final evaluation for the next A baseline. It is version
1.2: version 1 revision 2, fixed on 2026-10-27 after the human pools were built
and the evaluation metadata reserved, plus two amendments of the same day.
Amendment 1 records the Pangram budget and data-rights decisions. Amendment 2
adds eleven evaluation domains and the hosted frontier cohort, and restates the
data policy, the human-provenance rules and the Pangram cost model (see
Amendment record). Amendment 3 (2026-10-28) adds one further use of CAL, for the
mixed-int8 CPU serving build of the released weights.

At amendment 1:
- the data repair had rebuilt the five original pools and reservations
  (`splits-v2`);
- Dev-iter, Dev-select and CAL human prose for those five domains had been
  acquired by pipeline;
- positive generation had started with a Train pilot.

At amendment 2:
- `splits-v4` keeps every `splits-v2` and `splits-v3` assignment byte for byte
  and adds the per-blog regrouped blog splits, 23 new Train-side pools and the
  reservations of eleven new evaluation domains;
- Dev-iter, Dev-select and CAL human prose for FineWeb and Global Voices had
  been acquired by pipeline; no prose of the eleven new domains had been
  acquired for any evaluation split;
- the hosted frontier cohort had been collected on Train passages only.

At both amendments no Dev-select, CAL or Test document had been inspected or
scored, no Test prose had been acquired, and no arm had been fitted or scored
on the new splits.

The machine-readable copy is `protocol-v1.json` in the ignored stage directory
`data/grammar-private/a-competitive-20261027/`. Its SHA-256 is recorded at the
end of this document. The seal binds this document's commit, and through it
that hash. Any change after an evaluation split is opened needs a new protocol
version, and the opened split cannot be reused as fresh evidence.

Both earlier final Tests ([a-final-test.md](a-final-test.md) and the stage-3
Test in [a-encoder-adaptation.md](a-encoder-adaptation.md)) and every earlier
Development and Calibration population are consumed or exposed. They are
regression evidence only and select nothing here. On the stage-3 Test the
adapted A scores 0.07254665 balanced involvement Brier against 0.04802542 for
CAL-mapped Pangram, a gap of +0.02452124 [+0.00406349, +0.04100618].

## Claim

### Primary metric

The primary metric is the source-group/origin-balanced binary AI-involvement
Brier score. Involvement is `p_model + p_mixed`. The label is 1 for model-only
or mixed history and 0 for documented human-only text. Each source group has
equal mass. Each observed origin cell within a group has equal mass. Documents
within a cell have equal mass. The primary population is the Test human
documents plus their instruct-generated positives (mirrors, exact-span
rewrites and full rewrites), including the frontier Test positives of
[Frontier Test positives](#frontier-test-positives). The robustness slice and
the [diagnostic slices](#diagnostic-slices-ungated) are outside it. The
family-weighted Brier (equal group mass, observed origin mix) is reported
beside it and is not gated.

The metric is computed twice, and both results are gated:

- **Recipe:** the mean over the fits for fixed seeds 17, 29 and 41.
- **Released artifact:** the seed-17 fit alone, which is the released weight
  set.

The comparator is the Pangram version pinned on CAL (expected 4.1). Its
`fraction_ai + fraction_ai_assisted`, clamped to 1, is mapped to involvement
by the same clipped isotonic map that A receives (see Calibration), fitted on
CAL observations only. Pangram is observed only on the two pre-registered
random subsamples in [Pangram subsample](#pangram-subsample). A is scored on
every CAL and Test document.

### Tier C, competitive baseline

All four conditions must hold on the sealed Test, for the recipe and for the
released artifact.

1. **Noninferiority.** The upper end of the two-sided 95% interval of the
   balanced Brier difference (A minus Pangram) is at most δ = 0.010. The
   difference is the Horvitz-Thompson estimate of the full primary Test value
   from the Pangram Test subsample (see [Estimator](#estimator)). That end,
   `draws[974]`, is a one-sided 97.5% bound.
2. **Human false positives.** At the CAL operating point for 1% human FPR, A's
   human FPR over **all** Test humans has a one-sided 95% Clopper-Pearson
   upper bound of at most 2.5%. The flag limit follows from the realised
   number of Test humans (`pangram_subsample::max_flags`):
   - 29 flags for 1,600 humans, 38 for 2,000 and 60 for 3,000;
   - 64 for 3,170, the minimum, and 68 for 3,350, the planned 18-domain
     size.

   The condition must hold for each of the three seeds. The largest flag count
   is the headline figure. Pangram's FPR and TPR at its own CAL operating
   point are reported beside A's, on the subsample humans.
3. **Slices.** On each gated slice (see [Gated slices](#gated-slices)), the
   Horvitz-Thompson balanced Brier gap point estimate from the Test subsample
   is at most 0.030. Slice intervals are reported but not gated.
4. **Execution.** Every primary Test document returns a completed result.

### Tier I, improved baseline

Tier I is reported whether or not Tier C holds, and it is gated for both the
recipe and the released artifact. The baseline is the frozen stage-3 adapted
A. Its weights are unchanged, and it receives the same new-CAL calibration as
every arm.

- The selected A must beat it with the paired balanced Brier interval
  entirely below zero.
- The selected A's human FPR at the diagnostic 0.5 involvement cutoff must be
  no higher than the frozen model's.

### Reported, not gated

- Share: source-balanced MAE and MSE, overall and by realised-share bin
  ((0,10), [10,25), [25,50), [50,75), [75,90), [90,100), 100) and on mixed
  documents, against Pangram's raw-fraction MAE. The diagnostic target is a
  mixed-document MAE of at most 10 points. Share is qualified separately.
- Attribution: the current conditional human and model reporting, with
  `other_human` and `other_ai` fallbacks and every documented contributor role
  scored.
- TPR at the CAL-declared 1% and 0.1% human FPR points; involvement ECE10
  overall and on humans; human FPR at 0.5.
- A's balanced Brier, family-weighted Brier and every other A metric on the
  full primary Test, beside the subsample comparison.
- The robustness slice (base-model continuations and splices), scored and
  reported separately.
- The [diagnostic slices](#diagnostic-slices-ungated) and the domain slices
  that are not gated.
- **Frontier models.** Involvement Brier per frontier attribution label, and
  per-label attribution accuracy (top-1 model attribution among the frontier
  labels and `other_ai`) on held-out voices, for A, E0 and the frozen
  comparators. Pangram has no attribution output.
- **Style breadth.** Brier and human FPR per style axis and per brief cluster
  (positives) and per human genre and style domain (negatives), with the
  style and genre coverage of the Test humans reported next to the positives.
- Per-generator detection and attribution on held-out voices, beside the same
  measures on Train-partition voices where Dev-iter has them.
- Frozen V6 and the matched E0 control on the same Test.
- The family-weighted Brier and every per-seed result.

### Failures and missing observations

- **A execution failures.** This covers a crash, a timeout, a non-finite
  output, and an input over the 2,048-token encoder limit, which counts as
  unsupported. Each such document stays in every denominator.
  - Its involvement is scored at the worst case, `1 - label`.
  - A failed human counts as flagged, and a failed positive as missed.
  - Any failure on a primary Test document fails Tier C condition 4.
  - The same scoring applies to every A arm and frozen comparator.
- **Pangram observations.**
  - **Version.** The version string returned by the first successful CAL
    response is pinned. Any CAL or Test response with another version is
    treated as missing and is not re-requested.
  - **Text.** One observation is kept per exact UTF-8 text and version. A
    response whose echoed text differs from the submitted text, other than by
    whitespace, is a failure. Whitespace-only differences are accepted and
    recorded.
  - **No reuse.** Observations from earlier campaigns are never reused.
  - **Route.** Every observation goes through the bulk API. Jobs hold at most
    1,000 billable units each and are durable intents. Results are fetched
    and stored as soon as a job finishes, because the provider keeps them for
    only 48 hours. The collector is a new tracked tool adapted from the
    stage-3 true-bulk collector; its commit and binary hash are bound in the
    seal.
  - **Retries.** There are no automatic retries. An item with an uncertain
    outcome (no recorded result) may be re-sent once, in a new job, after
    reconciliation. A second uncertainty, any item failure, a budget
    interruption (HTTP 402) or a version mismatch makes that document
    missing. A job whose results expired before they were stored counts as
    uncertain for every item in it.
- **Missing Pangram on CAL.** Missing CAL subsample documents are left out
  of the Pangram map fit only. If more than 2% of the CAL documents selected
  for Pangram are missing, Tier C is reported as not evaluated.
- **Missing Pangram on Test.** A missing Test subsample document stays in the
  paired comparison with its inclusion weight, and Pangram is scored at its
  best case for that document: involvement equals the label. This is
  conservative for A. If more than 2% of the primary Test documents selected
  for Pangram are missing, Tier C is reported as not evaluated.
- If the [balance check](#balance-check) fails, Tier C is not evaluated. Tier
  I still is.

## Calibration

Every finalist, the E0 control, frozen V6 and frozen stage-3 A receive the
same rule. It is applied to each seed's fit separately, and it is fitted on
CAL only, after selection.

1. **Origin temperature.** One scalar temperature on the origin logits,
   fitted by balanced-weighted negative log-likelihood on CAL.
2. **Involvement map: `isotonic_v1_clipped`.** This is the balanced-weighted
   `isotonic_v1` procedure of
   `grammar/crates/grammar-a-baseline/examples/compare_supervised_pangram.rs`,
   applied to the temperature-scaled involvement:
   - Equal input scores are aggregated into one knot before pool-adjacent-
     violators.
   - Prediction is linear between adjacent unique CAL knots and constant
     outside the CAL support.
   - The output is clamped to `[1/(2·n_h), 1 − 1/(2·n_+)]`, where `n_h` and
     `n_+` are the numbers of CAL human and positive documents in the fit.
3. **Consistency.** `p_model` and `p_mixed` are scaled to the calibrated
   involvement, keeping their ratio. Share is projected into
   `[p_model, p_model + p_mixed]`.

The temperature-only involvement map replaces step 2 only on a recorded
mechanical failure of the isotonic fit: a non-finite value, an empty class or
a fit error. The choice is never made by comparing CAL, Dev or Test scores,
cross-fitted or otherwise.

The Pangram map is `isotonic_v1_clipped` on Pangram's clamped fraction over
the CAL subsample. Each document's fit weight is its full-CAL balanced weight
divided by its stratum's inclusion probability, so the map targets the same
balanced CAL population as A's. `n_h` and `n_+` are counted over the subsample
documents that have valid observations.

### Operating point

For each seed and each target FPR α (0.01 gated, 0.001 reported):

- The candidate set `V` is the distinct calibrated involvement values attained
  by CAL documents of any origin, plus +∞.
- The threshold `t` is the smallest `v` in `V` such that at most α of the CAL
  human documents have a calibrated value of at least `v`. Each human counts
  once, unweighted, and ties at `v` count as flagged.
- If no finite value qualifies, `t = +∞` and nothing is flagged.
- A Test document is flagged when its calibrated value is at least `t`. Test
  values come from the same map and can fall between CAL values. Both sides
  are compared as f64, and `t` is recorded as its exact bit pattern.
- Pangram's operating point uses the same rule on its own calibrated values,
  over the CAL subsample humans.

## Pangram subsample

### Budget

The Pangram budget is a fixed detector budget, recorded privately. It is
not increased, and spending never exceeds it. Costs are counted in
billed units:
- **Unit.** One started 100-word block per valid item, at least one per item
  (bulk billing for Pangram 4). The bulk unit price is 20% below the realtime
  price; the price used is recorded privately with the balance.
- **Planning length.** The 570 exposed Dev-iter humans (the five original
  domains, FineWeb and Global Voices) give the mean billed units per
  document in the power evidence. Positives are assumed to have the same
  length distribution.
- **Margin.** 10% of the balance is never planned for.
- **Not requested.** Pangram is never requested for Dev-iter, Dev-select,
  the robustness slice or the diagnostic slices.

### Strata

Strata come from pipeline metadata (domain, origin and realised share),
never from inspecting prose. The realised share is the label share, so a
mixed document whose realised share falls below 10% is in the (0,10) stratum
whatever bin was requested.

- **Test.** Domain × origin cell × realised-share bin over the primary Test
  documents:
  - `human_only`;
  - `model_only`;
  - `mixed` in each realised-share bin: (0,10), [10,25), [25,50), [50,75),
    [75,90), [90,100) and 100.

  That gives nine strata per domain and 162 over the 18 domains. A stratum
  key is `<domain>|human_only`, `<domain>|model_only` or
  `<domain>|mixed:<bin>`. Frontier, OLMo-2 and Train-family positives share
  their domain's strata.
- **CAL.** Domain × origin cell: `<domain>|human_only`,
  `<domain>|model_only` and `<domain>|mixed`, 51 strata over the 17 CAL
  domains. The CAL subsample only fits the comparator's map, and share bins
  would leave most of its 600 documents at the per-stratum floor.

Empty strata are dropped.

### Allocation

The two rules share one procedure, `neyman_allocation` in
`grammar/crates/grammar-a-baseline/src/pangram_subsample.rs`:
- `n_h` is proportional to `W_h · S_h`, where `W_h` is the stratum's share of
  the split's full balanced mass (equal group mass, equal observed-cell mass
  within a group, equal document mass within a cell).
- Every non-empty stratum takes at least `min(N_h, 3)` documents and at most
  `N_h`.
- The real-valued allocation is iterated. Free strata below their floor are
  held at the floor. Otherwise free strata above `N_h` are fixed at `N_h` and
  every floor hold is released. The remainder is then reallocated over the
  free strata until nothing moves.
- Free strata are rounded down. The deficit is placed one document at a time
  by largest fractional part, ties by ascending stratum key, never above
  `N_h`.

**CAL**: `n_cal` = 600 documents, with `S_h` = 1, which is proportional to the
balanced CAL mass over the 51 domain × origin-cell strata. At the planned CAL
(2,090 humans, about 6,800 primary documents) this gives 200 humans, 200
model-only documents and 200 mixed documents, about 1,615 billed units.

**Test**: Neyman allocation with these planning standard deviations of the
per-document paired difference:

| Cell | Stage-3 documents | Stage-3 SD | Planning `S_h` |
|---|---:|---:|---:|
| `human_only` | 40 | 0.1799 | 0.1799 |
| `model_only` | 120 | 0.0001 | 0.0500 (floor) |
| `mixed:(0,10)` | 51 | 0.3994 | 0.3994 |
| `mixed:[10,25)` | 63 | 0.3287 | 0.3287 |
| `mixed:[25,50)` | 118 | 0.1563 | 0.1563 |
| `mixed:[50,75)` | 39 | 0.0736 | 0.0736 |
| `mixed:[75,90)` | 86 | 0.0735 | 0.0735 |
| `mixed:[90,100)` | 0 | — | 0.0735 (larger of [75,90) and 100) |
| `mixed:100` | 119 | 0.0689 | 0.0689 |

How the planning values were set:
- The per-document difference is `(A − y)² − (Pangram − y)²`. A is the
  calibrated stage-3 adapted A and Pangram the CAL-mapped stage-3 Pangram, on
  the 636 stage-3 Test queries.
- Each SD is the larger of the three-seed-mean value and the seed-17 value.
  Strata are pooled over works and domains, since stage 3 had no domain
  variety.
- A bin with fewer than 20 stage-3 documents takes the larger SD of its
  nearest populated neighbour bins.
- Every planning SD is floored at 0.05. Model-only text saturated both systems
  on stage 3, and new generators may not, so no stratum is starved.
- The values are fixed now and are not re-estimated from new data.

### Sizes and the power calculation

The Test size has a pre-registered minimum. It is set by a planning power
calculation for the noninferiority gate with δ = 0.010, from the stage-3
per-document scores (counts and moments only). Amendment 2 re-ran it for the
18-domain Test; the planning SDs above are unchanged.

- **Variance model.** `Var = deff · Σ_h W_h² S_h² / n_h`. Documents are
  treated as independent within strata, with no finite-population
  correction.
- **Clustering.** `deff = 1 + ρ · f · Σ_g (1 − 1/M_g) / Σ_g (1/M_g)`, where
  `f` is the sampling fraction and `M_g` the documents in group `g`, with
  every group at its cap. This is the design effect of the balanced
  estimator, in which every group has equal mass: a large group carries only
  its own mass. For equal group sizes it equals amendment 1's
  document-weighted form `1 + ρ · f · (M − 1)`, which overstated the effect of
  the one-collection and few-prompt domains by orders of magnitude. ρ =
  0.0280 is the larger one-way-ANOVA intraclass correlation of
  stratum-centred residuals within the 20 stage-3 works (seed 17 0.0280,
  three-seed 0.0185).
- **Planned Test.** 3,350 humans with two primary positives each, plus one
  frontier positive for a quarter of them: about 10,900 primary documents.
  - Group counts for the five original domains come from `splits-v2`
    expected groups, and FineWeb has one document per group.
  - Global Voices has its Test cap of 15 documents per posting account in the
    design effect, and 4.17 documents per account (observed over the 375
    acquired Dev-iter, Dev-select and CAL humans, cap 8) in its mass.
  - Each other new domain uses its target divided by
    `max(1, min(cap, expected capped capacity / reserved groups))` from
    `splits-v4`.
  - OLMo-2 has one third of the open-model positives.
- **Realised-share mass.** Mixed strata are binned by realised share, so the
  (0,10) stratum is planned explicitly. Two bin-mass scenarios are computed,
  and the larger requirement is used:
  - `planned_span_fraction_v1`: the planned span word fraction of the 2,099
    Dev-iter, Dev-select and CAL mixed items in the local positive plan,
    which is the realised share of a length-preserving replacement. (0,10)
    has 0.5% of the mass;
  - `stage3_realised_v1`: the realised shares of the 476 stage-3 Test mixed
    documents. (0,10) has 10.7% and [90,100) none.
- **Targets.** At a true difference of 0, 80% power needs a standard error of
  at most δ/(z₀.₉₇₅ + z₀.₈₀) = 0.003569, and 90% power needs at most 0.003085.

| Requirement | `planned_span_fraction_v1` | `stage3_realised_v1` |
|---|---:|---:|
| Test strata | 162 | 144 |
| 80% power, true difference 0 | 1,245 | 1,436 |
| 90% power, true difference 0 | 1,620 | 1,892 |
| 80% power, true difference +0.005 | 5,013 | not reachable with every primary Test document observed |
| **Pre-registered minimum `n_min`** (the larger 90% figure, rounded up to 50) | **1,900** | **1,900** |

At `n_min`, under the `stage3_realised_v1` scenario:
- The planning SE is 0.00308, with deff 1.019.
- Power is 0.901 at a true difference of 0, 0.369 at +0.005, and about 0 at
  the stage-3 gap of +0.0245. Under `planned_span_fraction_v1` it is 0.942 and
  0.422.
- The minimum spend, CAL 600 plus Test 1,900, is about 6,730 billed units.
- **Slices.** The chance that a gated slice's point estimate exceeds 0.030
  when its true gap is 0.010:

| Slice | Planning SE | Chance |
|---|---:|---:|
| `held_out_generator` | 0.0046 | < 0.001 |
| `frontier` | 0.0071 | 0.002 |
| `wikipedia` | 0.0077 | 0.005 |
| `plos` | 0.0094 | 0.017 |
| `fineweb` | 0.0102 | 0.025 |
| `wikinews` | 0.0119 | 0.046 |
| `fineweb_blogs`, `reddit`, `instructional`, `peer_review` | 0.0123 | 0.051 |
| `wikipedia_talk`, `ubuntu_irc`, `wikisource` | 0.0123 | 0.052 |
| `project_gutenberg` | 0.0162 | 0.108 |
| `legal_opinions` | 0.0166 | 0.113 |
| `us_federal_register` | 0.0168 | 0.118 |
| `global_voices` | 0.0207 | 0.167 |

  The ungated domains would have planning SEs of 0.032
  (`spoken_transcripts`) and 0.039 (`student_essays`,
  `historical_manuscripts`), with chances of 0.27 to 0.31.

**Test size `n_test`.** The Test subsample takes the remainder of the
spendable balance, fixed at the balance check. It is never below `n_min` and
never above the realised primary Test population. A larger subsample
raises power at a true difference of +0.005. The design at the private
planning balance is kept only in the private power evidence.

These numbers are planning approximations. The new A, generators and domains
differ from stage 3, and the gate itself uses the bootstrap below. The
calculation is `grammar/crates/grammar-a-baseline/examples/a_comp_pangram_power.rs`.
Its plan and output are in the ignored stage directory under
`protocol-v1.2-evidence/` (SHA-256 values in `protocol-v1.json`). Amendment
1's plan and output stay under `protocol-v1.1-evidence/`.

### Selection

- **Seed:** 20261027.
- **Key:** `SHA-256("slopninja-a-comp-pangram-subsample-v1:20261027:" + split + ":" + d.id)`,
  with split `cal` or `test`.
- Within each stratum, the `n_h` documents with the smallest keys are
  selected.
- `N_h` and `W_h` are the realised values. CAL uses its primary documents once
  CAL positives exist. Test uses its primary documents after Test positives
  are generated and accepted, so terminal failures are not documents.
- The planning SDs, floors, `n_cal`, `n_min` and the `n_test` fixed at the
  balance check do not change.
- No document is substituted, and no stratum is resized after Test prose
  exists.

### Estimator

The inclusion probability of a selected document is `π_h = n_h / N_h`. Let
`w_d` be the full primary Test balanced weights, which sum to 1, and `G` the
number of Test groups. Each group gets the value

`v_g = G · Σ_{d ∈ g, selected} w_d · Δ_d / π_h`,

and a group with no selected document has `v_g = 0`.
- **Estimate.** The Horvitz-Thompson estimate of the full-Test balanced
  difference is the mean of `v_g` over all `G` groups.
- **Interval.** The protocol bootstrap resamples those `G` values: 1,000
  draws, xorshift seed 20261026, `draws[24]` and `draws[974]`.
- **Recipe and released artifact.** `Δ_d` is the paired difference averaged
  over seeds 17, 29 and 41 for the recipe, and the seed-17 difference for the
  released artifact.
- **Slices.** The slice estimate uses the same rule over the slice's own
  documents, groups and balanced weights, with each document's `π_h`.

The functions are `balanced_weights`, `ht_group_values`, `bootstrap_interval`
and `select`. Unit tests check the full-sample identity, unbiasedness over
every one-per-stratum sample, allocation edge cases, the selection key and
the Clopper-Pearson flag limits.

### Balance check

This happens immediately before Phase 3, once CAL positives exist and before
the first Pangram request.

1. **Read the balance `B`.** The provider has no balance API, so the
   operator reads the balance from the provider and re-confirms it before
   dispatch. Record the source, time and value privately. A minimal paid
   probe of public synthetic text is allowed, and its HTTP 402 behaviour
   shows whether credit remains; the probe's cost counts against `B`.
2. **Spendable units.** `U` = 0.90 · `B` divided by the bulk unit price. This
   keeps the 10% margin.
3. **CAL cost.** `U_cal` = Σ over the CAL subsample of
   `max(1, ceil(words / 100))`, with words counted by the protocol word
   policy.
4. **Test cost per document.** `û_test` = the mean billed units of all CAL
   primary documents.
5. **Size.** `n_test` = min(realised primary Test population,
   50 · floor((U − U_cal) / (50 · û_test))).
6. **Failure.** If `n_test` < 1,900, no Pangram request is made and Tier C is
   not evaluated; only Tier I is reported. Otherwise `n_test` and the balance
   record are bound in the seal before any Test prose exists.
7. **No figure.** If no balance figure can be obtained, `n_test` = `n_min`,
   and any later HTTP 402 is handled by the missing-observation rule. A probe
   that returns 402 means the balance is exhausted.

Pangram's word count may differ from the protocol count, and the margin
absorbs that. Every request is a durable bulk-job intent. Dispatch never
exceeds `U`: a job that would exceed what remains of `U` is not sent, and its
documents are missing.

## Decisions

The open questions were answered on 2026-10-27, and the answers are
recorded privately in the stage directory. The data-rights and scope
questions were first delegated, and the coordinating session's
recommendations were recorded as the decisions. Later decisions of the same
day revised Q1, Q4 and Q6; the table gives the decisions in force at
amendment 2.

| Question | Decision |
|---|---|
| Q1 Pangram coverage and budget | A fixed budget recorded privately and never increased, observed through the bulk API with a 10% margin and the operator's re-confirmation before dispatch. Stratified random CAL and Test subsamples, as in [Pangram subsample](#pangram-subsample). A is evaluated on the full Test |
| Q2 Meaning of "competitive" | Tier C noninferiority with δ = 0.010 plus Tier I, for the recipe and for the released artifact. Superiority is not claimed |
| Q3 Output scope | Involvement confidence gates. Share and attribution are reported only |
| Q4 Data rights | The [data policy](#data-policy) below: sources with non-commercial, research-only or data-mining-prohibiting terms are excluded from every split; CC BY-ND is evaluation-only; registration- or agreement-bound sources need the user's permission first. Stack Exchange and prose-licence PLOS are Train-side only. Mac-resident corpora are not copied |
| Q5 Serving envelope | E4 (ModernBERT-large) may be trained and compared. It can be selected only by the rule in Selection, where the tie rule prefers the smaller model. Serving qualification remains separate |
| Q6 Generators | The open-model roster below is pinned. Hosted frontier outputs are training and evaluation data in their own weighted cohort, and frontier identification is a primary goal. The Test covers the open models, the Test-only OLMo-2 family and frontier positives on Test passages generated after the seal |
| Q7 GPU coordination with the other session | Not answered; default in force. Shared host lock, `tools/gpu-lock.sh`, with a 30 s start watch. The other session's STAGE3 `generation-v1/serve.sh` checks memory once and does not take the lock |
| Q8 Other hosts | build-host only |
| Q9 OLMo-lineage exposure | OLMo-2 is accepted as the Test-only generator, with the caveat below that V6 saw OLMo-3 text. The hold-out is at checkpoint level, not family level |
| Q10 Stack Exchange route | The archive.org `stackexchange_20220606` dump, for Train and Train-mining only (see Sources) |

**Q9.** OLMo-3-7B-Instruct output is in detector-v5 and detector-v6:
- about 1,054 generated rows by the data mapper's count;
- 838 Olmo-3-7B-Instruct Train rows by the protocol reviewer's metadata scan;
- 1,042 rights entries in `datasets/detector-v6/data_rights.json`.

Another 18 rows are in a-supervised. So the V6 encoder has seen OLMo-3
output. OLMo-3 is a separate pretraining run from the same developer, and
whether it shares pretraining data with OLMo-2 was not reviewed.

The OLMo-2-13B-Instruct DPO preference mix also contains generations from
Train-family lineages:
- Mistral 7B Instruct v0.2;
- Gemma 2 9B and 27B;
- Qwen2.5 7B, 14B and 32B;
- Phi-3 and Phi-3.5 mini.

It further contains GPT-4 Turbo and GPT-4o outputs, and its SFT mix contains
GPT-4 WildChat responses. The held-out-generator slice is therefore held out
at the weight level, not at the data level. The user accepted this
checkpoint-level hold-out (Q9).

## Sources and labels

### Data policy

By the user's decision of 2026-10-27, A admits, for training and evaluation:
- public domain, CC0, CC BY, CC BY-SA, ODC-By and similar licences;
- copyrighted text that is lawfully accessible and carries no non-commercial
  restriction;
- terms that restrict only generative or LLM training;
- datasets whose terms ask only for citation.

CC BY-ND text is admitted for evaluation only, as an ungated diagnostic with
no derived positives.

Excluded from every split, including evaluation and diagnostics:
- CC BY-NC, BY-NC-SA and BY-NC-ND licences;
- research-only, academic-only, educational-only or non-commercial terms;
- signed or click-through agreements that bar commercial products or
  derived models;
- terms that forbid data mining outright.

Sources that need registration, an account or a signed or click-through
agreement are not acquired without the user's permission, even where the
terms might allow the use.

Every admitted source must satisfy all of these:
- **Lawful access.** No circumvention of logins, paywalls, rate limits or
  technical blocks, and no scraping against access controls.
- **Recorded terms.** The licence or terms, their snapshot date and access
  route, and the admission basis of the user's decision of 2026-10-27 are
  recorded in the source's admission record and on every row.
- **No redistribution.** Exact text stays in ignored storage. Publication in
  `datasets/` needs separate redistribution rights.
- **Removability.** Every row carries a per-source cohort id, so any source
  can be dropped and the model refitted.
- **Personal data.** Author ids are opaque grouping keys. No pseudonymous
  author is linked to a real identity.
- **Private material.** The AGENTS.md rules for private correspondence,
  coursework and the private campaign pool are unchanged, and such text is
  never sent to a hosted model.

A source withdrawn by a rights decision before the seal is removed by a
protocol amendment. Its reserved evaluation groups then leave every split and
nothing replaces them.

### Admitted human sources

| Source | Rights basis | Date rule | Use |
|---|---|---|---|
| Project Gutenberg books | US public domain from PG RDF, exact HTTP bodies retained | Public-domain historical | Train, mining, evaluation |
| Wikipedia | CC BY-SA | Revision timestamp before 2022-11-30 | Train, mining, evaluation |
| Wikinews | CC BY 2.5 (articles after the 2005 relicensing) | Publication and revision before 2022-11-30 | Train, mining, evaluation |
| PLOS articles with a licence link | CC BY or CC0 with a Creative Commons licence link | Publication before 2022-11-30 | Train, mining, evaluation |
| PLOS articles with a prose-only licence statement | CC BY or CC0: PLOS publishes all content under one of them by policy | Publication before 2022-11-30 | Train and mining only |
| US Federal Register | 17 U.S.C. 105 public domain | Publication before 2022-11-30 | Train, mining, evaluation |
| FineWeb (`HuggingFaceFW/fineweb`), including its blog-platform subset | ODC-By 1.0 for the dataset, subject to the Common Crawl terms of use | Crawl snapshot before 2022-11-30 | Train, mining, evaluation (`fineweb`, `fineweb_blogs`) |
| Global Voices | CC BY 3.0, with attribution kept per article | Publication before 2022-11-30 | Train, mining, evaluation |
| Stack Exchange, archive.org `stackexchange_20220606` dump | CC BY-SA: the dump's bundled licence is CC BY-SA 3.0, and each post's own licence (2.5, 3.0 or 4.0) is kept per row with attribution | Post dated before the dump, 2022-06-06 | Train and mining only |
| Reddit, Webis-TLDR-17 | CC BY 4.0 for the compilation; Reddit's terms on the user text are recorded | Post before 2017 (corpus 2006–2016) | Train, mining, evaluation (`reddit`) |
| Ubuntu IRC logs (Common Pile copy) | Public domain as released; no explicit licence on the log site, recorded | Log date before 2022-11-30 | Train, mining, evaluation (`ubuntu_irc`) |
| Wikipedia talk pages (WikiConv) | CC BY-SA | Comment timestamp before 2022-11-30 | Train, mining, evaluation (`wikipedia_talk`) |
| Wikisource, Wikibooks (with Cookbook), Wikivoyage | CC BY-SA 4.0 / GFDL | Revision before 2022-11-30 | Train, mining, evaluation (`wikisource`, `instructional`) |
| OpenReview and ORB reviews | CC BY 4.0 | Review before 2022-11-30 (ICLR up to 2022) | Train, mining, evaluation (`peer_review`) |
| F1000Research reports | CC BY | Report before 2022-11-30 | Train and mining only |
| CourtListener opinions and Caselaw Access Project | US judicial opinions, public domain (CourtListener Public Domain Mark; CAP taken upstream, not from a corrected-OCR copy) | Decision before 2022-11-30 | Train, mining, evaluation (`legal_opinions`) |
| FOMC transcripts, AMI meeting corpus | US government work; CC BY 4.0 (AMI, manual transcripts) | Meeting before 2022-11-30 | Train, mining, evaluation (`spoken_transcripts`) |
| ICSI meeting corpus | CC BY 4.0, manual transcripts | 2000–2002 | Train and mining only |
| GovInfo congressional hearings, Compilation of Presidential Documents | 17 U.S.C. 105 public domain; copyrighted inserts excluded | Publication before 2022-11-30 | Train and mining only (speakers span hearings) |
| LoC By the People transcriptions | Volunteer transcriptions released into the public domain; the underlying collection rights are recorded | Public-domain historical originals | Train, mining, evaluation (`historical_manuscripts`, Test only) |
| American Stories, British Library Heritage Made Digital newspapers | CC BY 4.0; CC0 | Public-domain historical | Train and mining only, capped |
| PERSUADE 2.0 | CC BY 4.0 | Collected before 2022-11-30 | Train, mining, evaluation (`student_essays`) |
| ELLIPSE | CC BY 4.0 | Collected before 2022-11-30 | Reserved prompts in `student_essays`; Train-side rows held (see [Diagnostic slices](#diagnostic-slices-ungated)) |
| Amazon Reviews 2023 | Citation only | Review before 2022-11-30 | Train and mining only |
| Santa Barbara Corpus of Spoken American English | CC BY-ND 3.0 US | ~1988–1996 | Evaluation only: ungated diagnostic, humans only |

The pool, admission record, licence snapshot and cohort id of each new source
are under `data/pools/` in the stage directory, and `splits-v4/summary.json`
counts them.

**Stack Exchange basis.**
- The archived 2022-06-06 copy is admitted under the CC BY-SA licence that
  its bundled `license.txt` states (3.0), with each post's own licence
  version recorded per row.
- The later download-route conditions noted in
  `docs/miner-training-sources.md` concern the current official download, not
  this archived copy. The admission record states this basis. When results
  are published, `docs/miner-training-sources.md` records it too.
- Stack Exchange is used in no evaluation split. Contributor closure would
  make Stack Exchange evaluation a skewed subpopulation.

**PLOS prose-only licences.** The 117,115 articles rejected earlier because
their CC BY statement was prose without a link are admitted. They are
Train-side only, since the PLOS evaluation reservations were already fixed.

**FineWeb.**
- Only crawl snapshots before 2022-11-30 are used. The text stays in ignored
  storage and is never redistributed.
- Pangram names FineWeb among its training sources, so the FineWeb slices
  favour the comparator. That is conservative for A, and the seal records it.
- A FineWeb page whose registered domain belongs to another admitted source,
  or to a hosted copy of one, is excluded from FineWeb. Examples are
  wikipedia.org, wikinews.org, plos.org, federalregister.gov, gutenberg.org,
  globalvoices.org and the Stack Exchange network.
- The blog subset (blog platforms and personal-blog domains) forms its own
  domain, `fineweb_blogs`, grouped by blog.

**Global Voices.** It is downloaded directly on build-host and never copied
from the Mac. Only English articles before 2022-11-30 are used.

**Not admitted:**
- By the commercial-use exclusion: Blog Authorship, PELIC, CORAAL, MediaSum,
  Goodreads, Yelp, Hacker News, ICNALE, EFCAMDAT, Lang-8, ICE, BNC, BAWE,
  MICUSP, Write & Improve and TOEFL11. Any downloaded copy is not admitted or
  rendered, and its raw download is deleted.
- ASAP 2.0 essays outside PERSUADE: no evidence that they were written
  before 2022-11-30.
- Full Reddit dumps: excluded (terms recorded in the admission record).
- Mac-resident corpora are not copied.

### Human provenance

- **Date.** A document is admitted only when its own timestamp (post,
  revision, comment, decision or meeting date, not the dump or crawl
  collection date) is before 2022-11-30, or when it is a public-domain
  historical text. Undated text is not admitted.
- **No machine-produced text on the human side.** The following are
  rejected: automatic speech recognition, machine translation, and OCR
  corrected by a language model or by an unstated method. Corpora whose
  correction method is unknown are taken from their upstream source, or
  not at all.
- **Unit.** Excerpts are whole-paragraph runs of 150–450 words. Short-form
  genres use the closest faithful unit, recorded in each admission record:
  - a whole post or comment (Reddit, Wikipedia talk, reviews);
  - one speaker's messages in one channel-day (IRC);
  - one speaker turn (meetings, hearings);
  - whole essays or reports.
- **Language.** A language screen (English stopword share) runs before
  admission.
- **Deduplication.** Exact and near-duplicate checks run against the prior
  index and every pool, as in [Overlap checks](#overlap-checks).
- **Proxy status.** Every admitted human document is a proxy for human-only
  writing: dated or historical text whose pretraining exposure for ModernBERT
  and Pangram is unknown. It is reported as its own stratum, never as an
  authenticated human-only workflow.
- **No comparator labels.** Pangram outputs never supply labels, choose
  prompts, generators or outputs, or mine negatives.

**OCR and transcription markup.** OCR errors, transcription markup and
platform boilerplate can make human text trivially separable from AI text.
The rules are:
- **Normalise.** Adapters normalise them before excerpting:
  - line unwrapping and lexicon-checked line-end hyphens;
  - dropping non-lexical transcription markup such as vocal-sound and
    disfluency tags;
  - removing trailing signatures and quoted replies.
- **Break runs.** Unintelligible, redacted, foreign or inserted material
  breaks a run rather than being kept.
- **Gate OCR quality.** OCR text is gated on legibility and on lexicon and
  odd-token rates, and each row records its OCR tier.
- **Record rates.** Each row records its residual rates (removed markup per
  100 words, OCR tier). Each admission record reports them per source.
- **Cap.** OCR newspaper text is capped in Train.
- **Report.** Human FPR and Brier are reported by OCR tier and by
  removed-markup presence. A gap between flagged and unflagged humans is a
  shortcut warning, recorded with the results.
- **Positives inherit noise.** Rewrites of a noisy human keep its noise
  outside the replaced spans, so noise occurs on both sides of the label.

### Hosted frontier cohort

Hosted frontier outputs are admitted for A training and evaluation under
`docs/a-hosted-output-admission.json`. Frontier identification is a primary
goal: per-model attribution that holds across voices, alongside involvement
and share.

- **Cohort.** `hosted_frontier_v1` (`docs/a-frontier-positives.md`), plus
  the 116 earlier captures that the manifest admits by record hash. Records
  are collected only on public passages of their own split: Train, and
  Dev-iter, Dev-select and CAL once their prose exists. Test passages are
  used only after the seal (see
  [Frontier Test positives](#frontier-test-positives)).
- **Weights.** The default policy is `hosted_frontier_label_balanced_v1`:
  every attribution label gets the same total weight, and the cohort's mean
  per-record weight is 1. A run may set any cohort or per-label weight,
  including 0. The run manifest records the count and weight used for every
  label, and a run without that entry is invalid. Cohort weights are a
  fitting choice decided on Dev-iter. They are not gated.
- **Identity labels.** When the provider reports the prose model, that name
  is the attribution label, for example `claude-opus-5-5`. When only the
  requested selector is known, the label is `requested:<selector>`, for
  example `requested:gpt-6-astra`. That basis is reported separately and
  never presented as a resolved snapshot. Helper models that appear only in
  usage accounting are not authors.
- **Roster.** Only general-purpose writing models are collected.
  Specialised security models are excluded.
- **Reasoning effort.** Medium for every hosted call, recorded per record.
  It is not a style axis.
- **Never used for selection.** Frontier records never select prompts,
  generators or outputs. Mechanical acceptance applies, with no fidelity
  review.

### Sources and source groups

A source is one work, article or document. A source group is the split and
bootstrap unit. No source group spans splits. The grouping rule
(`source_groups_v2`) is:

- **Project Gutenberg:** grouped by writer. Writers were reserved for
  evaluation before any of their text was rendered.
- **Wikipedia and Wikinews:** one article lineage per group. A redirect or
  move that joins articles across splits, found at acquisition, excludes the
  evaluation group whole.
- **PLOS:** one article per group. An evaluation article that shares a folded
  author name with any Train-side article, or with another split, is excluded
  rather than merged. Evaluation articles in one split that share an author
  are merged.
- **Federal Register:** one department and sub-agency office per group.
  - Offices with more than 2,000 Train-side documents are not evaluation
    candidates.
  - Train-side documents of evaluation offices are held out of Train.
- **FineWeb:** one registered domain (public suffix plus one label) per
  group, so one site never spans splits. A registered domain with evaluation
  documents has no Train-side text.
- **Global Voices:** one WordPress posting account per group.
  - Train-side articles by an evaluation account are held out of Train.
  - An evaluation article that shares any author with another split is
    excluded rather than merged.
- **Stack Exchange and prose-only PLOS:** Train-side only. Prose-only PLOS
  articles count as Train-side articles for the PLOS author-name closure.

The eleven domains added by amendment 2 are grouped by contributor, with
contributor buckets 900..1000 reserved before any of their text was rendered:

| Domain | Group key |
|---|---|
| `fineweb_blogs` | blog (`fineweb_blog:<host>` on blog platforms, otherwise `fineweb_site:<registered domain>`) |
| `reddit` | Reddit author (opaque hash) |
| `ubuntu_irc` | IRC nick (opaque hash) |
| `wikipedia_talk` | talk-page user (opaque hash) |
| `wikisource` | author where the work names one, otherwise the transcription index |
| `instructional` | Wikibooks book or Cookbook page; Wikivoyage page |
| `peer_review` | OpenReview forum (paper); reviewers are anonymised |
| `legal_opinions` | judge (CourtListener person or Caselaw Access Project judge) |
| `spoken_transcripts` | FOMC speaker; AMI participant component |
| `student_essays` | essay prompt (the releases carry no writer id) |
| `historical_manuscripts` | By the People collection |

No `splits-v4` group reuses a `splits-v3` group key. That rule guards the
shared `author:` keys of Wikisource and Gutenberg and the shared
`fineweb_site:` keys of the blog domain and FineWeb. Collaborative pages
(Wikisource, Wikibooks, Wikivoyage) are not one author's style.

Blocking replaces transitive merging so that prolific contributors cannot form
a giant component. It has costs:
- 16,900 of 19,792 reserved PLOS articles (85%) were blocked by author.
- 37,679 reserved Federal Register documents were blocked by office size.
- 39,125 Train-side Federal Register documents were held out. The
  `splits-v2` repair holds out 27,003.

It also shifts the evaluation populations. Evaluation PLOS comes from authors
without Train-side text, and evaluation Federal Register text comes from small
offices. These shifts are reported with the Test results.

These limits remain:
- PLOS name variants (initials against full names) are not matched.
- Wikipedia and Wikinews have no contributor separation.
- FineWeb has no author separation beyond the registered domain.
- One judge can appear under both a CourtListener and a Caselaw Access
  Project key, and these are not matched.
- Hearing speakers span hearings, so hearings and presidential documents are
  Train-side only.
- `student_essays` (3 Test prompts) and `historical_manuscripts` (one Test
  collection) are nearly single-cluster domains.

### Positives

Positives are descendants of their split's own human documents.

- **Mirrors.** From-scratch writing, topic- and length-matched, one per human
  document.
- **Exact-span rewrites** replace whole-sentence runs chosen by digest.
- **Full rewrites** rewrite the whole excerpt. A full rewrite of a human
  source is mixed history with share 1.
- **Requested-fraction bins** are R1 [10,25), R2 [25,50), R3 [50,75), R4
  [75,90), R5 [90,100) and R6 full.
  - Requests below 10% are not made, because one sentence of a 150–450-word
    excerpt usually exceeds 10% of its words.
  - Realised shares below 10% are kept and reported in the (0,10) bin.
- **Labels** use the realised final-word fraction in model-produced spans,
  never the requested fraction.
- **Robustness slice.** Base-checkpoint continuations and splices are kept
  separate in CAL and Test and may enter Train.
- **Accounting.** Raw attempts, terminal failures, and duplicate or no-change
  exclusions stay in the accounting.
- **Length.** Documents are not truncated after generation.
- **Negative control.** A same-text no-op splice reconstruction is retained
  as a mechanical negative control.

## Splits

There are 18 evaluation domains:
- the five original domains: `project_gutenberg`, `wikipedia`, `wikinews`,
  `plos` and `us_federal_register`;
- `fineweb` and `global_voices`, added by amendment 1;
- eleven domains added by amendment 2: `fineweb_blogs`, `reddit`,
  `ubuntu_irc`, `wikipedia_talk`, `wikisource`, `instructional`,
  `peer_review`, `legal_opinions`, `spoken_transcripts`, `student_essays`
  and `historical_manuscripts`.

| Split | Seven earlier domains (PG / Wikipedia / Wikinews / PLOS / FR / FineWeb / Global Voices) | Each new domain | `spoken_transcripts` | `historical_manuscripts` | Total | Minimum | Use |
|---|---|---:|---:|---:|---:|---:|---|
| Dev-iter | 85 / 160 / 25 / 85 / 65 / 75 / 75 = 570 | 50 | 25 | — | 1,045 | 990 | Every iterative decision; exposed by design |
| Dev-select | 170 / 320 / 50 / 170 / 130 / 150 / 150 = 1,140 | 100 | 50 | — | 2,090 | 1,980 | Opened once to choose among at most four finalists |
| CAL | 170 / 320 / 50 / 170 / 130 / 150 / 150 = 1,140 | 100 | 50 | — | 2,090 | 1,980 | Calibration, operating point, named-profile thresholds, Pangram map (on its subsample) |
| Test | 260 / 380 / 160 / 260 / 200 / 220 / 220 = 1,700 | 150 | 150 | 150 | 3,350 | 3,170 | One shot after the seal |

"Each new domain" covers the nine new domains other than
`spoken_transcripts` and `historical_manuscripts`.
- **Spoken transcripts.** The smaller targets follow from the reserved FOMC
  speakers and AMI participant components, about 343 expected capped
  documents.
- **Historical manuscripts.** There is one reserved By the People
  collection, so it is Test only.
- **Positives.** Every evaluation human gets two primary positives, plus a
  frontier positive for a quarter of them (see
  [Positive construction](#positive-construction-all-evaluation-splits)).
- **Train.** At least 20,000 human documents from at least 2,000 sources over
  the Train-side sources above, and a mining pool of at least 100,000.
  `splits-v4` realises 42,852 Train humans (30,000 from `splits-v3`, 3,300
  blog documents and 9,552 from the new pools) and 2,712,986 mining
  documents. Train positives are the local-model Train plan plus the hosted
  frontier cohort.

The five original domains' targets, the Gutenberg cap and the backfill rule
are those of the `splits-v2` data repair. They were set before any
evaluation prose was acquired, and they replace revision 2's values:
- Dev-iter was 85 / 100 / 35 / 100 / 90;
- Dev-select and CAL were 175 / 200 / 70 / 200 / 175;
- Test was 250 / 300 / 180 / 300 / 250;
- the Gutenberg cap was 2 documents per writer;
- backfill went to Wikipedia and then PLOS.

**Reservation.** Every added domain is reserved from its own never-rendered
contributor buckets, with contributor and domain closure against Train. Each
is reserved at about 1.3 times expected capped capacity, in successor
reservations that keep every earlier assignment unchanged: `splits-v3` for
FineWeb and Global Voices, `splits-v4` for the eleven domains of amendment 2.
- **Minimums.** Each minimum is the amendment-1 minimum plus 95% of the new
  domains' targets, rounded down to a multiple of 10.
- **Spoken transcripts** is reserved below the 1.3 buffer: Test 182, CAL 59
  and Dev-select 60 expected capped documents.
- **Global Voices** Dev-iter, Dev-select and CAL groups come from contributor
  buckets 700..900. A superseded Train-side run had rendered those buckets by
  pipeline only; no text was printed, read or scored. That text was deleted
  with receipts, and the Train side was re-rendered from buckets 0..700.
  Global Voices Test comes from buckets 900..1000.
- **Global Voices near-duplicates.** 48 acquired Dev-iter (16), Dev-select
  (13) and CAL (19) candidates share a 12-word window with Global Voices
  Train text (`splits-v4/checks/`). They are excluded at selection, without
  replacement.

### Reservation and acquisition

- Evaluation metadata was reserved from family buckets 900..1000. Those
  buckets were never rendered for Train-side pools. Project Gutenberg was
  reserved at the writer level.
- **Excerpts.** Each split's human documents are taken by the sealed pool
  adapters, using the adapter commit and binary hashes bound in the seal:
  - whole-paragraph runs of 150–450 words;
  - the adapter's deterministic passage rule;
  - at most these documents per group: 3 for PG, 1 for Wikipedia, 1 for
    Wikinews, 2 for PLOS, 5 for the Federal Register and 1 for FineWeb;
  - for Global Voices, 8 in Dev-iter, Dev-select and CAL and 15 in Test.
    `splits-v3` declared these caps on metadata before reservation, as the
    smallest caps that reach 1.3 times the targets. The Dev-iter, Dev-select
    and CAL prose was acquired with them by pipeline before this amendment
    and has not been opened. Amendment 2 adopts them in place of the
    earlier cap of 2;
  - for the amendment-2 domains: 1 for `fineweb_blogs`, `reddit` and
    `wikipedia_talk`; 2 for `ubuntu_irc`, `instructional`, `wikisource` and
    `peer_review`; 3 for `legal_opinions`; 20 for `spoken_transcripts`; 200
    for `student_essays`; 220 for `historical_manuscripts`;
  - any other reservation that records different caps needs an amendment
    before its prose is opened;
  - deduplication against the prior index, the Train-side pools and the other
    splits.

  No agent chooses passages.
- **Order.** Documents are taken per domain in the reserved `groups.jsonl`
  order and stop at the domain target.
- **Backfill.** One of the five original domains that exhausts its reserved
  groups below target passes its deficit to the same split's Wikipedia
  groups, continuing in reserved order. FineWeb, Global Voices and the
  amendment-2 domains have no backfill, and a domain that runs out is
  evaluated at its realised size. Backfill never draws on another split or on
  reserved-unassigned groups. A split that still falls below its minimum is
  evaluated at its realised size, and flag limits use the realised human
  count.
- **Opening.** Prose is *opened* when any agent or person reads, samples,
  prints or scores it, or when a model is prompted with it. Sealed pipeline
  code may render, hash, deduplicate and overlap-check prose, and pipeline
  generation may prompt models with it, without opening it for inspection.
  The only outputs anyone examines from that are counts, hashes and receipts.
- **Use.** Dev-select prose may be acquired and used for pipeline generation
  before the finalists are bound. Nobody inspects or scores it until the
  single Dev-select evaluation. Test prose is acquired only after the seal.
- **Withheld contributors.** No evaluation group has a contributor with
  Train-side text, by construction above and within the limits listed
  there.
- **Mining pool.** It is Train-only. Evaluation documents are never mined,
  never borrowed to fill Train quotas, and never used as mirror topics outside
  their own split.

### Gated slices

A slice is gated only where the 15-group and 150-human minimum is reachable.
For a domain, reachable means its Test reservation holds at least
1.3 × 15 = 20 groups and at least 1.3 × 150 = 195 expected capped human
documents. That is the minimum with the reservation's own buffer. The list
is fixed now:

- the seven earlier domains: `project_gutenberg`, `wikipedia`, `wikinews`,
  `plos`, `us_federal_register`, `fineweb` and `global_voices`;
- eight amendment-2 domains: `fineweb_blogs` (479 reserved Test groups),
  `reddit` (301), `ubuntu_irc` (236), `wikipedia_talk` (537), `wikisource`
  (1,866), `instructional` (569), `peer_review` (232) and `legal_opinions`
  (103), each with 195 expected capped documents;
- `held_out_generator`: Test human documents plus OLMo-2-13B-Instruct
  descendants, balanced like the primary metric;
- `frontier`: Test human documents plus the frontier Test positives, pooled
  over frontier models and balanced like the primary metric.

These domains are reported but not gated:
- `spoken_transcripts`: 19 groups and 182 expected documents, below the
  buffer;
- `student_essays`: 3 prompt groups;
- `historical_manuscripts`: one collection.

They stay in the primary population, and their humans count toward the FPR
gate.

**Realised minimum.** After Test acquisition and generation, and before any
Test document is scored, pipeline counts decide each listed slice. A listed
slice that ends below 15 groups or 150 human documents, or for `frontier`
below 150 frontier positives, is reported ungated, and its shortfall is
recorded. Nothing else removes a slice from the list.

Every slice gap is the Horvitz-Thompson estimate from the Test subsample. The
robustness slice and the diagnostic slices below are reported separately and
are not gated.

### Diagnostic slices (ungated)

These are reported separately, outside the primary population and every
gate. Pangram is not requested for them. Each is scored by A, E0 and the
frozen comparators.

- **`sbcsae_spoken_conversation`.** The 866 rendered Santa Barbara Corpus
  documents (CC BY-ND), humans only, with no derived positives. It reports
  human FPR at the CAL operating points and mean involvement. Its 4 reserved
  recordings stay unrendered.
- **`ellipse_ell_learner_essays`.** The 4,599 held ELLIPSE essays by
  English-language learners, humans only. They are neither Train nor mining
  until the user decides between Train and a held-out fairness slice. A
  later decision to train on them moves them to Train with a recorded
  amendment and removes this diagnostic.
- **Learner demographics.** Human FPR in `student_essays` and in the ELLIPSE
  diagnostic, by English-learner status, grade and the other released
  demographic fields, from metadata.
- **Frontier per model.** `frontier:<label>` for each frontier attribution
  label: involvement Brier and attribution accuracy on held-out voices.
  Plain and voiced frontier positives are also reported separately.
- **Held-out styles.** Test humans plus every Test positive written in a
  Test-partition voice. Results are reported per style axis and brief
  cluster.
- **OCR and markup.** Human FPR and Brier by OCR tier and by removed-markup
  presence (see [Human provenance](#human-provenance)).

CORAAL is not a diagnostic slice. Its licence is non-commercial, so it is
excluded from every split. The 116 earlier hosted captures were drafted from
earlier Train source families and are admitted as training data, so they are
not an evaluation slice. Any of them that overlaps an evaluation group is
excluded with a receipt.

## Generator roster

A generator family is a pretraining lineage.
- **Weight level.** A model fine-tuned or distilled at the weight level from
  another family's base belongs to both families.
- **Data level.** A model post-trained on another family's generated outputs
  keeps its own family. That output exposure is recorded and reported with
  the slice it affects.

Every generator records its repository, revision, file SHA-256, quantization,
runtime and decoding settings before generation. The new Train families and
the CAL-only family are pinned in `generators/MODELS.lock`
(SHA-256 `0d0190d5ef94ec87c8fff3a621984980ef29f9b39b798a28ddca236e8ba34dac`),
with licence evidence beside it. The runtime is llama.cpp b11756
(`llama-server` SHA-256 `462ea7f6…`).

| Role | Instruct checkpoints | Base checkpoints |
|---|---|---|
| Train, Dev-iter, Dev-select | Qwen3.6-35B-A3B, Phi-4-mini, Mistral-7B-v0.3, Gemma-3-4b, Granite 4.0 H-Tiny, SmolLM3-3B, ERNIE-4.5-21B-A3B; gpt-oss-20b in Train only | Granite 4.0 H-Tiny base, SmolLM3-3B base, ERNIE-4.5-21B-A3B base (Train only) |
| CAL only | GLM-4-9B-0414 (Q4_K_M) | — |
| Test only | OLMo-2-1124-13B-Instruct | OLMo-2-1124-13B |

The Test-only files are pinned now, and their LFS SHA-256 values come from the
Hugging Face API without downloading:

- **Instruct:** `allenai/OLMo-2-1124-13B-Instruct-GGUF` @
  `02bfa6da698648c2472c57727cd0098049fe3ec8`, file
  `olmo-2-1124-13B-instruct-Q8_0.gguf`, 14,579,450,336 bytes, SHA-256
  `125bfb27fdd8f946878599ee92b150efd7ba072e6d5d6662e404f3827a73158d`. It is a
  first-party conversion of `allenai/OLMo-2-1124-13B-Instruct` @
  `3a5c85baefbb1896a54d56fe2e76c0395627ddf4`.
- **Base:** `allenai/OLMo-2-1124-13B-GGUF` @
  `2ac13832b20ee9960068cc67eb881a270eea3628`, file
  `OLMo-2-1124-13B-Q8_0.gguf`, 14,579,449,024 bytes, SHA-256
  `99fe16ca8dd470601dffa87b0939665f870d8e447d392959de7ed9e032b4cba3`. It is a
  first-party conversion of `allenai/OLMo-2-1124-13B` @
  `3fefddc1bf18a30e1d9b91000271630718f2aa8b`.
- **Why 13B.** Both checkpoints come from one release with first-party GGUFs.
  Q8_0 is near-lossless and fits the 24 GB GPU with room for the KV cache. The
  0325-32B release has no first-party base GGUF.

**OLMo-2 licence and card review**, card-only, 2026-10-27; evidence in
`protocol-v1r2-evidence/`:
- Both repositories declare Apache-2.0, and both cards say OLMo 2 "is intended
  for research and educational use" under Ai2's Responsible Use Guidelines.
- The instruct card states that the model "has been fine-tuned using a
  dataset mix with outputs generated from third party models and are subject
  to additional terms: Gemma Terms of Use".
- The project already generates with Gemma-3-4b under the Gemma Terms of Use.
- The post-training data cards also note:
  - OpenAI terms on GPT-4 outputs;
  - non-commercial subsets (No Robots, CC BY-NC 4.0);
  - "separate terms governing their use" for third-party outputs.
- The base checkpoint has no such statement.
- OLMo-2 output is used only as Test evaluation documents. It is never
  trained on and is not published in `datasets/`.

**Rules:**
- OLMo-2 files are not downloaded until the seal step.
- No OLMo-lineage output enters Train, Dev-iter, Dev-select or CAL, including
  retained earlier rows.
- GLM-4 output enters only CAL. It never enters fitting or selection.
- The local review model (Qwen3.8-27B) is not a generator.
- Hosted frontier models are a separate roster: the attribution labels of
  the [hosted frontier cohort](#hosted-frontier-cohort). The frontier Test
  roster is pinned in the seal. It is every label with at least 100 accepted
  Train records in the cohort's latest publish before the seal, in ascending
  label order. Frontier models are used for Train, Dev-iter, Dev-select, CAL
  and, after the seal, Test positives.

## Positive construction (all evaluation splits)

Every Dev-iter, Dev-select, CAL and Test human document `d` gets exactly two
open-model primary positive requests: one mirror and one mixed rewrite. A
quarter of them also get one frontier positive (see
[Frontier Test positives](#frontier-test-positives)). Assignments are
mechanical. Each key `k_x(d)` is
`SHA-256("slopninja-a-comp-plan-v1:" + x + ":" + d.id)`. Ranks are taken
within `d`'s split and domain, in ascending key order.

### Generator assignment

Generator slots form a 21-slot cycle. The mirror generator is slot
`rank(k_mirror_gen) mod 21`, and the mixed generator is slot
`rank(k_mixed_gen) mod 21`.

| Split | Slots 0–6 | Slots 7–20 |
|---|---|---|
| Test | OLMo-2-13B-Instruct (one third) | Train instruct families, two slots each, in this order: Qwen3.6, Phi-4-mini, Mistral-7B-v0.3, Gemma-3-4b, Granite, SmolLM3, ERNIE |
| CAL | GLM-4-9B-0414 (one third) | As for Test |
| Dev-iter, Dev-select | The same seven Train families, three slots each, in the same order | — |

gpt-oss-20b is not an evaluation generator.

### Test positive quotas

Ranks are taken within split and domain, so every domain gets its own cycle.
For `n` humans in a domain, OLMo-2 receives `7·floor(n/21) + min(n mod 21, 7)`
mirror requests and the same number of mixed requests. Each requested bin
receives one sixth of the mixed requests, up to rounding by rank. At the
planned Test targets:

| Domain | Test humans | Mirror requests | Mixed requests | OLMo-2 instruct requests (mirror + mixed) | Frontier positives | Robustness (OLMo-2 base) |
|---|---:|---:|---:|---:|---:|---:|
| `project_gutenberg` | 260 | 260 | 260 | 91 + 91 | 65 | 130 |
| `wikipedia` | 380 | 380 | 380 | 128 + 128 | 95 | 190 |
| `wikinews` | 160 | 160 | 160 | 56 + 56 | 40 | 80 |
| `plos` | 260 | 260 | 260 | 91 + 91 | 65 | 130 |
| `us_federal_register` | 200 | 200 | 200 | 70 + 70 | 50 | 100 |
| `fineweb` | 220 | 220 | 220 | 77 + 77 | 55 | 110 |
| `global_voices` | 220 | 220 | 220 | 77 + 77 | 55 | 110 |
| `fineweb_blogs` | 150 | 150 | 150 | 52 + 52 | 38 | 75 |
| `reddit` | 150 | 150 | 150 | 52 + 52 | 38 | 75 |
| `ubuntu_irc` | 150 | 150 | 150 | 52 + 52 | 38 | 75 |
| `wikipedia_talk` | 150 | 150 | 150 | 52 + 52 | 38 | 75 |
| `wikisource` | 150 | 150 | 150 | 52 + 52 | 38 | 75 |
| `instructional` | 150 | 150 | 150 | 52 + 52 | 38 | 75 |
| `peer_review` | 150 | 150 | 150 | 52 + 52 | 38 | 75 |
| `legal_opinions` | 150 | 150 | 150 | 52 + 52 | 38 | 75 |
| `spoken_transcripts` | 150 | 150 | 150 | 52 + 52 | 38 | 75 |
| `student_essays` | 150 | 150 | 150 | 52 + 52 | 38 | 75 |
| `historical_manuscripts` | 150 | 150 | 150 | 52 + 52 | 38 | 75 |
| **Total** | **3,350** | **3,350** | **3,350** | **1,162 + 1,162 = 2,324 of 6,700** | **843** | **1,675** |

The realised counts follow the realised humans by the same rule. Frontier
positives are `ceil(n/4)` per domain. CAL, Dev-select and Dev-iter documents
in the new domains get the same requests per human under their own split's
cycle.

### Frontier Test positives

Frontier positives are generated for every evaluation split by the same
mechanical rule, with that split's own keys and ranks. For Test they are
generated only after the seal.

- **Who.** A human `d` gets one frontier positive when
  `rank(k_frontier(d)) mod 4 == 0`.
- **Model.** The model is roster slot `rank(k_frontier_gen) mod L` of the
  frontier roster pinned in the seal (`L` labels in ascending order).
- **Kind.** `rank(k_frontier_kind) mod 2` chooses a mirror or a mixed
  rewrite.
  - A mixed rewrite takes bin `[R1, R2, R3, R4, R5, R6][rank(k_frontier_bin) mod 6]`.
  - It follows the frontier collector's span rules: a single run, and R5
    keeps one whole sentence at the start or the end.
  - Labels use the realised share.
- **Voice.** The positive is voiced unless
  `rank(k_frontier_voice) mod 4 == 0`.
  - Voices are drawn by the collector's digest rule only from the split's own
    style-library partition (`dev_iter`, `dev_select`, `cal` or `test`).
  - Voices already used in records of another split are excluded, and so are
    imitated authors that match an evaluation author group.
- **Test voices.** The `test` partition is built before the seal by one
  further round of the library's invention process. It keeps only items
  whose clusters, at the library's clustering threshold, are new to every
  existing partition, and it needs at least 500 voice prompts and 300
  briefs. Its digests are bound in the seal, and it is never used before
  the seal. If the round falls short, the `dev_select` partition supplies
  the rest. Those voices are held out from Train and CAL but were seen in
  selection, and this is recorded.
- **Calls.** Medium reasoning effort, through the frontier collector at the
  commit and binary hash bound in the seal. The collector applies the
  protocol acceptance rules below.
  - A refusal or a second failure is a retained terminal failure. No other
    model, bin or voice is substituted.
  - Rate-limit and quota stops pause collection and are not failures.
  - A model that becomes unavailable before its items finish leaves those
    items as terminal failures.
  - Test evaluation starts only when every frontier item is accepted or
    terminal.
- **Population.** Frontier positives are primary documents. They share their
  cell's mass in the balanced weights and their domain's Pangram strata.

### Mixed bin and spans

- The mixed bin is `[R1, R2, R3, R4, R5, R6][rank(k_mixed_bin) mod 6]`.
- **Sentences** come from the sealed Rust splitter.
- **Bins R1–R4.**
  - Target fraction: the bin midpoint (17.5%, 37.5%, 62.5% or 82.5%).
  - One contiguous run of whole sentences whose word fraction is closest to
    the target, with the shorter run on ties.
  - The run is anchored at the start, middle or end by the first byte of
    `SHA-256(d.id + ":" + bin)` mod 3.
- **Two spans.** In R2–R4, documents with `rank(k_multi_span) mod 3 == 0` use
  two runs instead. Each takes half the target, one anchored at the start and
  one at the end, with at least one human sentence between them. Where that
  cannot fit, a single run is used.
- **R5.** Every sentence is replaced except one whole sentence, kept at the
  start, middle or end by the same digest.
- **R6.** The whole excerpt is rewritten in one request.
- Each span is a separate request.

### Prompt templates

Prompts are sent through each model's embedded chat template, with thinking
disabled.

**Mirror.** `{register}` is set per domain:
- `project_gutenberg`: "a passage from a published book";
- `wikipedia`: "a section of an encyclopedia article";
- `wikinews`: "a news report";
- `plos`: "a passage from a scientific research article";
- `us_federal_register`: "a passage from a US federal government notice or
  rule";
- `fineweb`: "a passage from a web page";
- `global_voices`: "an article from an international citizen-media news
  site";
- `fineweb_blogs`: "a post from a personal blog";
- `reddit`: "a post on an online discussion forum";
- `ubuntu_irc`: "a set of one person's messages in a technical-support chat
  channel";
- `wikipedia_talk`: "a comment on an encyclopedia article's discussion
  page";
- `wikisource`: "a passage from a historical published text";
- `instructional`: "a passage from a travel guide, recipe or how-to guide";
- `peer_review`: "a peer review of a scientific paper";
- `legal_opinions`: "a passage from a court opinion";
- `spoken_transcripts`: "a speaker's turn from a meeting transcript";
- `student_essays`: "an argumentative essay by a secondary-school student";
- `historical_manuscripts`: "a passage from a historical letter, diary or
  personal paper".

Frontier mirrors and rewrites use the frontier collector's templates, with
the same registers and the voice appended when the item is voiced.

`{words}` is the human excerpt's word count. `{title}` is the source title
from metadata; the "Title or subject" sentence is omitted when there is none.
`{terms}` lists the eight highest TF-IDF word types of the excerpt, chosen by
sealed Rust code:
- lowercased alphabetic types of at least four letters;
- a fixed stopword list removed;
- IDF taken from the domain's Train pool;
- ties broken by first occurrence.

```
Write {register} in English, about {words} words long. Title or subject: {title}. Cover these key terms: {terms}. Write natural, original prose. Return only the passage, with no title, heading, preface, analysis, or explanation.
```

**Span or full rewrite:**

```
Rewrite the following English passage in your own words while preserving its meaning, facts, perspective, register, and approximate length. Return only the replacement prose, retaining paragraph breaks where useful: no title, preface, analysis, quotation delimiters, or explanation.

{span}
```

### Decoding

- **Profile.** `rank(k_decoding) mod 2` selects profile A (temperature 0.7,
  top-p 0.9) or B (temperature 1.0, top-p 0.95).
- **Fixed settings:** top-k off, min-p 0, repeat penalty 1.0, `max_tokens`
  1536.
- **Seed:** the first four bytes of the request's SHA-256, read as a
  big-endian u32.
- **Server flags** are those in `generators/ROSTER.md`. Reasoning is off for
  every instruct model that supports turning it off.

### Acceptance

A response is accepted when all of these hold:
- its trimmed content is non-empty;
- it contains no chat-template special tokens;
- its word count is within [0.5, 1.5] of the target (span or mirror words);
- it is not byte-identical to the replaced span;
- it is not a duplicate;
- the assembled document fits in 2,048 A-tokenizer tokens.

Otherwise one more request is made with seed + 1. After two failures the item
is a retained terminal failure. No other generator, bin or text is
substituted. A human document whose positives fail stays in the split with
fewer observed origin cells.

### Robustness slice (Test only)

Test human documents with `rank(k_robust) mod 2 == 0` get one OLMo-2-13B base
document. `floor(rank(k_robust) / 2) mod 2` chooses its type.

- **Continuation.** The prefix is the shortest whole-sentence prefix with at
  least 30% of the words. It is sent verbatim, with no template. The output is
  cut at the last sentence end that keeps the document within the human
  word count. The document is prefix plus continuation.
- **Splice.**
  - Prefix: the shortest whole-sentence prefix with at least 25% of the words.
  - Suffix: the shortest whole-sentence suffix with at least 25%.
  - The middle is generated from the prefix and cut at the last sentence end
    that does not exceed the removed words.
  - The document is prefix, then generated middle, then suffix.
- **Decoding and acceptance.** Temperature 0.8, top-p 0.95, `max_tokens`
  1024, the same seed rule. A continuation needs at least 20 words, and the
  other acceptance checks apply.

Labels use the realised share. These documents are outside the primary
population.

Training positives follow the same templates and bins. Their decoding varies
within declared ranges, and their generator mix is a fitting choice that is
not gated.

## Fitting and selection

### Arms

The fixed comparators are frozen V6 and frozen stage-3 adapted A. Their
weights are unchanged, and they receive the calibration above.

| Arm | Change | GPU-hour ceiling, 3 seeds |
|---|---|---:|
| E0 | Stage-3 recipe (final-block adaptation) on the new Train; control | 3 |
| E1 | Full fine-tune from V6, document heads, Dev-iter early stopping | 12 |
| E2 | E1 plus a token-span involvement head from exact replacement spans, with attention or max aggregation; share from the projected token mean or a four-bucket expectation | 14 |
| E3 | The better of E1 and E2 by Dev-iter, plus up to three hard-negative mining rounds over the Train mining pool, scored by A only | 40 |
| E4 | ModernBERT-large with the E3 winner's recipe; permitted by the user (Q5) | 35 |
| Controls | Text-only and CFG-only inputs at reduced budget; origin-only and attribution-only controls for the mutual-benefit comparison | 4 |

- An arm that reaches its ceiling stops and is reported as budget-limited.
- An arm is the three seed fits. It is never a best seed and never an
  ensemble. Seed ensembles conflict with the one-model contract. A single
  distilled student remains possible as a later arm.
- Each arm runs at most 4 epochs at the new scale. The chosen epoch has the
  lowest uncalibrated balanced Dev-iter Brier, the earliest on an exact tie.
- Learning-rate screening uses at most three encoder rates per arm. Each is a
  one-epoch seed-17 run scored on Dev-iter. All three seeds then run fully at
  the chosen rate, and the arm's reported numbers come from those runs.
- Mining uses A's own scores on independently labelled Train human text.
  - Each round adds the mined false positives and topic- and length-matched
    mirrors of them.
  - Mining stops after the first round whose mean Dev-iter balanced Brier
    falls by less than 0.001, and after three rounds at most.
- **Style-robust objectives.** These are Dev-iter choices within E1 and E2,
  not separate arms. Each is compared with and without on Dev-iter by a
  one-epoch seed-17 run, and the time counts against the arm's ceiling.
  - **Style invariance:** style-balanced batches plus an auxiliary
    adversarial head with gradient reversal that predicts the style axis or
    brief cluster from the shared encoding.
  - **Generator contrast:** a supervised contrastive objective on the shared
    encoding. Its positives are records from the same generator in different
    voices; its negatives include other generators in the same voice and
    humans. Batches mix voices within a generator.

  Generator attribution accuracy on Dev-iter voices that are held out from
  Train is reported for both comparisons.
- **Frontier cohort weight.** It is chosen on Dev-iter from the default
  policy and at most two alternatives, and recorded in the run manifest.
- Distillation from Pangram and CFG fusion are not arms.

### Selection

1. Every iterative choice uses Dev-iter only: rates, epochs, head variants,
   the E1/E2 choice for E3 and the mining stop.
2. **Finalists.** There are at most four: E1, E2, E3 and either E4 or E0.
   - The user permitted E4 (Q5). If it completes all three seeds within its
     ceiling, it replaces E0 as a finalist. Otherwise E0 stays a finalist.
   - E0 is always fitted, calibrated and reported on the Test as the matched
     control.
   - Selecting E4 does not qualify it for serving. Serving qualification is
     separate.

   The finalists' frozen configurations and all seed weights are hashed
   before Dev-select opens.
3. **Dev-select.** It opens once. The finalist with the lowest three-seed
   mean uncalibrated balanced involvement Brier is selected.
4. **Tie rule.** It compares only the top two. If the paired Dev-select
   interval between them includes zero, the one earlier in this fixed order
   wins: E0, E1, E3 built on E1, E2, E3 built on E2, E4. The order follows
   serving footprint by architecture (E2 adds the span head; E4 is a larger
   encoder) and then simplicity. Nothing is measured to break a tie.
5. Dev-iter versus Dev-select rank agreement among the finalists is reported
   as a diagnostic of selection reliability.
6. After selection, CAL alone fits the calibration above, the operating
   points, the named-profile thresholds and the Pangram map.

If the Dev-select evaluation fails mechanically before any result is seen
(for example a crash), the identical frozen evaluation is rerun. A scoring
defect found after results were seen is handled by the post-hoc rule below,
except that the corrected evaluation makes the selection. The defect is
recorded in the seal.

### Uncertainty

- **Bootstrap.** Intervals use 1,000 paired source-group bootstrap draws with
  the xorshift generator seeded 20261026. `draws[24]` and `draws[974]` bound
  the two-sided 95% interval, as in `compare_supervised_pangram.rs`.
- **Seeds.** For the recipe, paired per-group differences are averaged over
  the three seeds before groups are resampled. For the released artifact,
  only seed 17 is used.
- **Exclusions.** The intervals exclude seed, training-data and
  calibration-estimation uncertainty.
- **Clopper-Pearson.** It treats documents as independent, so a source-group
  bootstrap of the FPR is reported beside it.
- **Multiplicity.** Gates use these intervals without a multiple-comparison
  correction, because the arms and slices are fixed here.

### Post-hoc defects

If a defect in evaluation code or data handling is found after the single
Test evaluation:
- The original result stays primary and is reported.
- A corrected run on the same sealed inputs is reported beside it and
  labelled post-hoc.
- A claim stands only if both the original and the corrected result pass.
- A correction can withdraw a claim but cannot create one.
- The Test stays consumed.

## Overlap checks

Every reserved or acquired source and every generated document is checked
against all prior material:

- published snapshots in `datasets/`: a-balanced, a-encoder, a-retained-train,
  a-supervised, a-train-augmentation, detector-v5, detector-v6,
  detector-hosted-v1, detector-origin-v1 and detector-revisions-v1;
- both consumed final Tests and the fresh Pangram Test of 2026-09-13;
- the stage-2 and stage-3 Development and Calibration populations;
- the style-rewrite exports, the CFG archive and the private A stage
  directories, read only;
- stage-3 catalog-inventory candidates and broadened-source acquisitions that
  the other session has opened or reserved;
- every Train-side pool of this protocol, including Stack Exchange, the
  prose-only PLOS articles, FineWeb, Global Voices, the blog pools and the
  23 `splits-v4` pools (`prior-registry-data4-v1`);
- the held diagnostic rows (ELLIPSE, SBCSAE);
- the hosted frontier cohort, before its records enter any split;
- the other new splits of this protocol.

The checks are:
- work and article aliases;
- exact and whitespace-normalized SHA-256;
- shared 50-word sequences;
- after generation, generated-text and prompt overlap.

The prior index must contain every file its registry names. A registry pattern
that matches no file is an error, not an empty set. A conflicting source group
is excluded whole, with a receipt. The checks run at reservation, again
immediately before the seal, and again after Test acquisition.

## Seal

The seal is written before any Test prose is acquired or any Test positive is
generated. It binds:

- this document's commit, and through it the `protocol-v1.json` SHA-256 below;
- all three seed weights of every finalist and of E0, plus frozen V6 and
  stage-3 A;
- the selection record with its Dev-iter and Dev-select results;
- the CAL calibration maps, operating points (as f64 bits) and named-profile
  thresholds;
- the Pangram CAL map and its pinned version;
- the Pangram subsample design:
  - the seed, strata, allocation rules, planning SDs, `n_cal` and `n_min`;
  - the CAL subsample selection;
  - the balance record and the resulting `n_test`;
  - the power evidence `protocol-v1.2-evidence/pangram-power-v2/power.json`;
  - the bulk collector's commit and binary hash;
- the FineWeb comparator-exposure note (Pangram names FineWeb among its
  training sources), which covers `fineweb` and `fineweb_blogs`;
- δ, the Tier C and Tier I thresholds and the gated slice list above;
- the Test reservation metadata (`splits-v4`), the adapter commits and
  binary hashes, the admission record and cohort id of every source, and the
  overlap receipts;
- the frontier plan:
  - the frontier Test roster;
  - the `test` voice partition's digests;
  - the frontier collector's commit and binary hash;
  - the cohort-weight policy;
- the generator plan:
  - `generators/MODELS.lock`, the Train-family generator configs and the
    OLMo-2 pins above;
  - the generation binary hash;
- an OLMo-2 qualification receipt. As the last step before the seal, both
  OLMo-2 files are downloaded to a Test-only directory, checked against the
  pinned SHA-256 values, loaded in the pinned runtime and given a 20-token
  smoke on a fixed synthetic prompt. If either fails, the seal is not written
  and a new protocol version is needed;
- the evaluation code commit, the binary hashes and the CUDA environment lock.

Then Test prose is acquired, and Test positives (open-model and frontier) are
generated. Next the realised slice minimums are decided from counts, the
audits run, and the evaluation runs once. The Test is marked consumed
afterwards, whatever the result.

## Execution

- Compute runs on build-host only, as durable `systemd-run --user` units named
  `slop-a-comp-*` in `slop-a-comp.slice`, with `MemoryMax`,
  `MemorySwapMax=0` and an exit file. There is one Cargo job at a time. Jobs
  above 6 GB hold the host's big-memory lock, and at least 400 GB stay free
  on `/`.
- Corpus handling, assembly and statistics are Rust. PyTorch trainers live in
  `grammar/crates/grammar-eval/python/`.
- Private text, fitted weights and receipts stay in
  `data/grammar-private/a-competitive-20261027/`. Tracked files carry counts,
  hashes and metrics only.
- Tests issue no live detector or model calls.

### GPU lock

The GPU is shared with another session. Every GPU job runs inside
`tools/gpu-lock.sh` for its whole lifetime:

```
systemd-run --user --unit=slop-a-comp-<name> -p MemoryMax=16G -p MemorySwapMax=0 \
  --working-directory=. \
  tools/gpu-lock.sh --wait --exit-file <dir>/exit -- <command>
```

- **Lock.** The helper takes an exclusive `flock` on
  `data/locks/gpu.lock`. The path is resolved
  through the Git common directory, so every worktree reaches the same file.
  If resolution fails or gives another path, the helper refuses with status 2.
  `--lock` is accepted only when it names that file.
- **Start check.** It then requires `nvidia-smi` `memory.used` below 500 MiB
  on every GPU. Without `--wait` a failed check exits 75. With `--wait` the
  helper releases the lock and retries every 60 seconds (`--interval`).
- **Start watch.** For 30 seconds after the start (`--watch`), the helper
  polls every 2 seconds. It aborts its own command with status 76 when:
  - a compute process outside its process tree or its own `.service` cgroup
    appears;
  - memory not attributed to its own processes stays at or above 500 MiB on
    two consecutive polls;
  - a query fails.

  The command is not restarted.
- **Signals.** TERM, INT and HUP are forwarded to the command's process group.
  The group is killed after a 30-second grace period, and the status is
  128 + signal.
- **Exit file.** `--exit-file` is written atomically on every path:
  - the command's status;
  - 2 for usage or configuration errors, including an exit-file directory
    that is missing or not writable, which is checked before locking;
  - 70 for an internal error;
  - 75 for a refusal;
  - 76 for an aborted race;
  - 128 + N for signal N.
- **Queries.** Every `nvidia-smi` query runs under a 30-second timeout.
- **Other processes.** The helper signals only its own command's process
  group. It never stops another process.
- **Lifetime.** The lock is held until every process that inherited its
  descriptor exits, including a server the command leaves running.
- **Memory caps.** The helper does not cap memory. Trainers keep their own
  `torch.cuda.set_per_process_memory_fraction` caps. Generation servers
  (`llama-server`) have no per-process CUDA cap, so the lock and the start
  watch are their only exclusion.
- **Tests.** `tools/tests/gpu-lock-test.sh` tests the helper with a fake
  `nvidia-smi` and a scratch lock.

## Amendment record

Amendment 1 (2026-10-27), before any Dev-select, CAL or Test document was
inspected or scored and before any Test prose existed:
- The Pangram budget is the fixed amount recorded privately, observed on stratified
  random CAL and Test subsamples (see [Pangram subsample](#pangram-subsample)).
  A is evaluated on the full Test, and its FPR gate uses all Test humans.
- The data-rights decisions are recorded:
  - Stack Exchange and prose-licence PLOS are Train-side only;
  - FineWeb and Global Voices are new evaluation domains;
  - Blog Authorship and PELIC are excluded;
  - hosted captures form an ungated robustness slice (replaced by
    amendment 2);
  - E4 is permitted under the tie rule;
  - OLMo-2 is accepted with the OLMo-3 caveat.
- The domain list, split targets, slice gates and Test positive quotas cover
  seven domains.
- The `splits-v2` data-repair targets, the Gutenberg cap of three and
  Wikipedia-only backfill replace revision 2's values.

Amendment 2 (2026-10-27), before any Dev-select, CAL or Test document was
inspected or scored, before any Test prose existed, and before any prose of
the eleven new domains was acquired for an evaluation split:
- **Data policy.** The data policy of 2026-10-27, with its
  commercial-use exclusions, permission rule and removability, replaces the
  amendment-1 list of exclusions. The Stack Exchange basis is restated as the
  dump's CC BY-SA 3.0 with per-post 2.5, 3.0 or 4.0 licences.
- **Human provenance.** The provenance rules and the OCR and
  transcription-markup normalisation rules are stated, with per-source rates
  recorded and reported.
- **New domains.** Eleven evaluation domains from `splits-v4`, with targets,
  minimums, caps and no backfill. Global Voices' caps of 8 and 15 are
  adopted.
- **Slices.** Gated slices only where the 15-group and 150-human minimum is
  reachable, plus the `frontier` slice. Listed slices that fall short at
  realised counts are reported ungated. Ungated domain and diagnostic
  slices are defined.
- **Frontier cohort.** Hosted frontier outputs are training and evaluation
  data in a weighted cohort with reported or requested-selector labels. The
  statements that hosted captures never enter Train and that there is no
  hosted generation budget are removed. Frontier positives join every
  evaluation split's primary population, Test ones only after the seal and
  in held-out voices. OLMo-2 stays the Test-only open-weight family.
- **Pangram cost model.** The budget is recorded privately and costed in
  bulk units with a 10% margin and the operator's re-confirmation. CAL
  strata are domain × origin cell.
- **Power.** The power calculation was re-run with the 18-domain Test, the
  observed Global Voices clustering, an explicit (0,10) realised-share
  stratum and the balanced-estimator design effect. `n_min` is 1,900.
- **Training.** Style-robust objectives and the frontier cohort weight are
  Dev-iter choices within the arms.

Amendment 3 (2026-10-28, int8 serving qualification), forward-only, recorded
before any int8 CAL output existed:
- **Purpose.** CAL is used for exactly one further purpose: fitting the
  protocol calibrator (`cal-policy-v3`, including its
  recorded-mechanical-failure temperature fallback) and the 1% and 0.1% CAL
  operating points of the mixed-int8 CPU serving build of the released seed-17
  weights.
- **Fallback build.** If the primary recipe fails, the same is done once for
  the single pre-declared fallback build.
- **What it requires.** Scoring every CAL document with that build, and
  reporting that fit's CAL reliability.
- **What it excludes.** No comparison, selection, tuning or gate uses CAL
  values. The release's f32 calibration and operating points are unchanged.
  Dev-select is not used, and Test is not touched.

## Machine-readable copy

`data/grammar-private/a-competitive-20261027/protocol-v1.json`:
- **Version 1.2** (amendment 2) has SHA-256
  `9ca96f5228c8a40671bb7c2c64d125cb587c7bfe534c3974cb0bf502966c8fab`.
- **Version 1.1** (amendment 1) had SHA-256
  `873c8af2b0286a5fe783a6cdf11683bde3aadd2dc07e3581c2887af3dc417520`. It is
  bound by commit `1a137389` and archived as
  `protocol-v1.2-evidence/protocol-v1.1.json`.
- **Version 1, revision 2** had SHA-256
  `2891ad6995426afcfbb20f80693ef8740a164b51e8447c817ad6d4ded1fb8719`. It is
  bound by commit `b79ffd2c` and archived as
  `protocol-v1.1-evidence/protocol-v1.r2.json`.
