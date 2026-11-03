# CFG writer vectors versus A encoder profiles: results

Measured on build-host on October 31, 2026, under the
[frozen protocol](cfg-vs-encoder-protocol.md) `cfg-vs-encoder-20261031`. Two
independent verifiers checked the run the same day. A
[follow-up](#follow-up-cfg-on-top-of-encoder-plus-stylometry) under protocol
version 2 then tested CFG on top of the encoder and the stylometric control, on
the same evaluation authors. This note gives aggregates only. Per-pair scores,
the selection records and the full metric files are in ignored storage under
`data/grammar-private/cfg-vs-encoder-20261031/eval-v1/` and `eval-v2/`. Commit
IDs here are the branch commits that the records bind; the
[alias map](cfg-vs-encoder-aliases.md) gives their IDs on main.

**Naming.** Arms A to F are the letters of this test. Arm A (CFG) is not the A
detector, and arm C (stylometric control) is not the C design component (shared
CFG contributions). This note always writes "arm" for an arm and "component C"
for the design component.

## Answer

| Question | Evidence |
|---|---|
| Do CFG writer vectors beat encoder writer profiles? | **No.** Arm A is below arm B on the primary verification endpoint: Δ AUC −0.034 [−0.047, −0.022]. On author fit the difference is unresolved: −0.009 [−0.047, +0.031]. |
| Do they add to encoder profiles? | **Yes, slightly, on verification only.** Fusing CFG with the encoder (arm E) raises the primary AUC by +0.027 [+0.022, +0.032] (97.5% interval [+0.021, +0.033]). On author fit the gain is unresolved: +0.007 [−0.017, +0.031]. |
| Outcome under the frozen rule | **CFG adds value.** CFG does not win clearly. |
| Context | The stylometric control alone (arm C) beats the encoder by +0.051 [+0.039, +0.063], and its fusion with the encoder (arm D) by +0.069 [+0.062, +0.076]. Both gains are larger than CFG's. |
| Verification | Both verifiers found that the outcome holds. One verifier found an unlogged departure from the frozen pooling text. Rescoring under the frozen wording gives the same outcome (see [Verification](#verification)). |
| Follow-up: does CFG add to encoder plus stylometry? | **Yes, by a resolved but very small amount, on verification only.** Adding CFG to arm D's fusion (arm F) raises the primary AUC by +0.00146 [+0.00035, +0.00253]. The follow-up re-used the evaluation authors above, so it is not independent of them. On author fit, F − D is −0.0010 [−0.0056, +0.0037] (descriptive). See [Follow-up](#follow-up-cfg-on-top-of-encoder-plus-stylometry). |

Bracketed values are paired author-bootstrap 95% intervals unless marked
otherwise. "Δ" is the first arm minus the second.

## Methods

**Freeze and execution (UTC).** The protocol was frozen at 15:40 and committed at
15:43 (`34d2fe3a`), before any metric it governs was computed. The `FROZEN.json`
SHA-256 is `4a33a404b8986a5075fa24e51e1f0c5906ef9c5f970b586b28f0bf3b670944af`.
Features were extracted between 16:00 and 23:02. Every feature manifest records
that no metric was computed. The evaluator clarifications were logged at 23:26.
Two rehearsals on training authors only followed. The evaluator binary
(`4e4fe0cd…`, built from `25f8cd9c`) was recorded at 23:30:54 and the training
selection at 23:31:19. The single evaluation ran from 23:31 to 23:32. At
evaluation time the evaluator recomputed the training selection and would not
continue unless it matched the record and its own binary hash. Code is
based on `origin/main` `3aa51700`. Evaluation used 4 threads, a peak of 1.6 GB RSS
and 58 s of wall time. No step used Dev-select, CAL or the consumed Test split.
No step made a hosted-model, external detector or Pangram call.

**Arms.** Training authors chose every configuration by mean 5-fold T1 AUC at k = 5.

| Arm | Representation | Candidates (training CV AUC, scorer S1 / S2) | Selected |
|---|---|---|---|
| A, CFG writer vector | Frozen exporter's expected counts under the supervised-forest numeric projection (`log1p` rate, training mean and SD, SD floor 0.01) | `leaf_grammar` 0.653 / 0.650; `structure` 0.641 / 0.638; `all` 0.645 / 0.643; `leaf_grammar_pool_merged_v1` 0.754 / 0.746; `all_pool_merged_v1` 0.745 / 0.736 | pool-merged grammatical leaves, S1: 756 coordinates, 740 at the SD floor |
| B, R0 encoder profile | R0 (`E1aux`, seed 17), FP64 CPU runtime | `h_raw` 0.795 / 0.790; `z_shared` 0.791 / 0.787 | `h_raw`, S1: 768 coordinates |
| C, stylometric control | 203 function words and the 2,000 most frequent character 3-grams | 0.836 / 0.832 | S1: 2,203 coordinates |
| D, fusion of B and C | Logistic fusion of impostor-normalized scores | weights: B 0.793, C 1.014 | fitted out-of-fold on training authors |
| E, fusion of A and B | same | weights: A 0.657, B 0.992 | same |

S1 is cosine on z-scores that use the total SD. S2 divides by the pooled
within-author SD. Pool merging, which sums each key over the exporter's window
pools, was decisive for arm A (0.754 against 0.653 unmerged). Fusion weights,
impostor normalization and T2 thresholds were fitted on training authors only.

**Pooling as run.** The evaluator unit-normalizes each standardized reference
vector, averages the results, and scores a query by cosine with that mean. The
frozen text says to L2-normalize the mean of the standardized reference vectors
instead. This departure was not logged before evaluation. It applies equally to
every arm. It changes no k = 1 score of a single arm (A, B or C). The fused arms
change at every k, because their weights are fitted on k = 5 trials (see
[Verification](#verification) and the
[pooling addendum](#pooling-addendum-summary)).

**Tasks.**

- **T1, verification.** Each query is scored against its own author's profile
  (genuine) and against up to 100 same-domain evaluation authors (impostors),
  with k = 1, 3 and 5 references. The primary endpoint is pooled author-weighted
  AUC at k = 5.
- **T2, identification.** Each domain has a gallery of enrolled evaluation
  authors. 131 of the 630 evaluation authors are withheld from the galleries, and
  499 are enrolled. A rejection threshold is set at a 10% false-accept target on
  310 simulated-withheld training queries.
- **T3, author fit.** For each target writer, rewrites toward that writer should
  outscore rewrites toward other targets and the unrewritten sources of the
  writer's full rewrites. The primary endpoint is the mean AUC per target.

**Statistics.** Paired author bootstrap: 2,000 replicates, ChaCha20 keyed by the
SHA-256 of `cfg-vs-encoder-20261031/bootstrap/v1`, and type-7 percentile
intervals. T1 and T2 resample query authors within domain, with impostor and
gallery profiles fixed. T3 resamples targets. Every arm shares the same
replicates.

**Decision rule.** No margin is tuned.

- **CFG wins clearly** if both the T1 and T3 intervals for A − B lie above zero.
- Otherwise, **CFG adds value** if the T1 or T3 interval for E − B lies above
  zero.
- Otherwise, there is **no evidence for CFG**.

## Data and exposure

**T1 and T2 populations.** Authors are Train-mining writers, assigned to the
training, evaluation and feasibility-only roles by a fixed hash. Each author has
up to 3 queries and 5 references, and no query shares a source group with its
author's references. Queries have 150 to 450 words.

| Domain (author key) | Evaluation authors | Queries | Withheld from T2 | Training authors |
|---|---:|---:|---:|---:|
| Stack Exchange (site user) | 300 | 900 | 63 | 200 |
| CourtListener (judge) | 200 | 600 | 38 | 150 |
| Web blogs (blog host, a weak label) | 89 | 267 | 18 | 83 |
| PLOS blogs (writer) | 18 | 54 | 5 | 22 |
| FOMC transcripts (speaker) | 16 | 48 | 5 | 14 |
| Project Gutenberg (author, cross-work) | 7 | 8 | 2 | 5 |
| **Total** | **630** | **1,877** | **131** | **474** |

Evaluation authors contributed 5,027 documents and training authors 3,782. No
author was excluded for an execution failure.

**Exposure per arm.** The protocol requires that none of the selected writers'
own documents was seen by either system. The verifiers confirmed this from
metadata. None of the 1,104 selected writers has a document in the A Train split.
None of their documents has a text hash or source group in R0's trainer bundle.
No document or text hash is shared between training and evaluation authors.
Seven Stack Exchange threads contain posts by both a training writer and an
evaluation writer. These are different people, so no identity crosses roles.
Each arm does have indirect exposure:

| Exposure | Arm A, CFG | Arm B, R0 encoder | Arm C, stylometric control |
|---|---|---|---|
| Same domain, other authors | Grammar development pools included Blog Authorship, PLOS One, and a 47-document FineWeb 2021-43 pilot. The pilot comes from the same collection as the web-blog hosts. Host overlap with the evaluated blogs was not checked. | R0 was trained on other authors' human rows in these domains: 600 Stack Exchange, 500 CourtListener, 6,080 Gutenberg, 300 FOMC, 300 PLOS-blog and 3,000 FineWeb-blog, plus their AI positives. | None. The arm has no learned weights. |
| Pretraining | None | The base encoder's pretraining exposure is unknown, and web, book and court text are likely. | None |
| Fitted statistics | Vocabulary and normalization from training authors | Normalization from training authors | Vocabulary and normalization from training authors |

The same-domain exposure is larger for arm B, which may favour the encoder.

**T3 population.** No five-post style-transfer rewrites exist, and no voice-fit
measure exists. The nearest data are hosted-model records in a voice that a local
model derived from one Gutenberg passage by the target writer. Full rewrites
rewrite another writer's passage. Mirrors are written from scratch to match a
passage's topic, length and key terms. Each profile has five of the target's own
Train-side documents from other source groups.

| T3 stratum | Targets | Records | Exposure to R0 |
|---|---:|---:|---|
| Primary: hosted full rewrites and mirrors | 249 | 438 (97 full, 341 mirror; 55 own-source pairs) | not in R0's training bundle; 129 targets have other text in A Train |
| Hosted span rewrites | 311 | 599 | not in the bundle |
| Records in R0's training bundle | 577 | 1,502 | trained |
| Local Dev-iter records | 54 | 91 | selection-exposed, not trained |

## Coverage

| Arm | Coverage on the 15,546 selected documents |
|---|---|
| A, CFG export | 4.28M parser tokens. 58.1% of tokens are in complete windows (35.0% native), 37.2% in partial windows and 4.7% in resource-limited windows. 0.17% of tokens are unresolved. 547 documents (3.5%) parse completely. No execution, projection or budget failures. |
| B, R0 encoder | All 15,546 documents supported (4.82M tokens). None truncated. |
| C, stylometric control | All 15,546 documents (3.62M word tokens). |

Full parsing is rare, so the primary comparison uses all text, as a deployed CFG
writer vector would. The coverage-restricted comparison feeds every arm only the
text of windows that parsed completely. A document qualifies when that text has
at least 100 tokens; 7,327 of 8,809 T1/T2 documents (83.2%) do. An author
qualifies when all five references and at least one query qualify: 293 of 630
evaluation authors (780 queries, 2,245 documents) and 226 of 474 training
authors. The restricted refit selected the same configurations (training CV:
A 0.713, B 0.757, C 0.808).

## Results

### T1, verification

**AUC by number of references (pooled, author-weighted).**

| Arm | k = 1 | k = 3 | k = 5 (primary) |
|---|---|---|---|
| A, CFG | 0.661 [0.647, 0.674] | 0.721 [0.709, 0.732] | 0.743 [0.732, 0.754] |
| B, R0 encoder | 0.693 [0.679, 0.707] | 0.754 [0.742, 0.766] | 0.778 [0.766, 0.788] |
| C, stylometric control | 0.738 [0.725, 0.750] | 0.808 [0.798, 0.818] | 0.828 [0.819, 0.838] |
| D, fusion B + C | 0.761 [0.749, 0.772] | 0.826 [0.817, 0.836] | 0.846 [0.838, 0.856] |
| E, fusion A + B | 0.715 [0.702, 0.730] | 0.782 [0.771, 0.792] | 0.805 [0.795, 0.815] |

**Error rates at k = 5.**

| Arm | EER | FPR at TPR 0.90 | FPR at TPR 0.95 |
|---|---|---|---|
| A, CFG | 0.325 [0.315, 0.336] | 0.638 [0.617, 0.664] | 0.763 [0.733, 0.784] |
| B, R0 encoder | 0.292 [0.281, 0.304] | 0.560 [0.536, 0.593] | 0.675 [0.643, 0.719] |
| C, stylometric control | 0.254 [0.244, 0.264] | 0.454 [0.428, 0.478] | 0.594 [0.543, 0.652] |
| D, fusion B + C | 0.233 [0.224, 0.243] | 0.419 [0.394, 0.447] | 0.553 [0.516, 0.579] |
| E, fusion A + B | 0.270 [0.260, 0.281] | 0.501 [0.478, 0.524] | 0.633 [0.593, 0.675] |

**Paired AUC contrasts.**

| Contrast | k = 1 | k = 3 | k = 5 |
|---|---|---|---|
| A − B | −0.032 [−0.049, −0.017] | −0.033 [−0.046, −0.020] | −0.034 [−0.047, −0.022] |
| E − B | +0.023 [+0.016, +0.029] | +0.028 [+0.022, +0.033] | +0.027 [+0.022, +0.032] |
| E − A | +0.054 [+0.044, +0.065] | +0.061 [+0.052, +0.069] | +0.061 [+0.053, +0.070] |
| C − B | +0.045 [+0.029, +0.060] | +0.054 [+0.041, +0.067] | +0.051 [+0.039, +0.063] |
| D − B | +0.068 [+0.058, +0.077] | +0.072 [+0.065, +0.081] | +0.069 [+0.062, +0.076] |
| A − C | −0.077 [−0.090, −0.063] | −0.087 [−0.099, −0.076] | −0.085 [−0.096, −0.074] |

At k = 5, A − B is +0.032 [+0.020, +0.046] in EER and +0.078 [+0.039, +0.112] in
FPR at TPR 0.90. E − B is −0.022 [−0.029, −0.014] and −0.059 [−0.085, −0.039].
For these two metrics lower is better, so arm A is worse than arm B and arm E is
better.

**By domain, AUC at k = 5.**

| Domain (evaluation authors) | A | B | C | D | E | A − B | E − B |
|---|---|---|---|---|---|---|---|
| Stack Exchange (300) | 0.809 [0.794, 0.823] | 0.841 [0.828, 0.856] | 0.924 [0.913, 0.933] | 0.938 [0.930, 0.947] | 0.883 [0.871, 0.894] | −0.033 [−0.052, −0.015] | +0.041 [+0.034, +0.048] |
| CourtListener (200) | 0.661 [0.638, 0.683] | 0.701 [0.682, 0.721] | 0.700 [0.678, 0.722] | 0.731 [0.711, 0.752] | 0.712 [0.693, 0.732] | −0.040 [−0.062, −0.018] | +0.011 [+0.001, +0.022] |
| Web blogs (89) | 0.762 [0.731, 0.792] | 0.836 [0.804, 0.867] | 0.834 [0.810, 0.861] | 0.872 [0.845, 0.899] | 0.851 [0.821, 0.880] | −0.074 [−0.106, −0.040] | +0.016 [+0.003, +0.028] |
| PLOS blogs (18) | 0.657 [0.582, 0.737] | 0.684 [0.609, 0.765] | 0.783 [0.709, 0.854] | 0.802 [0.726, 0.872] | 0.713 [0.633, 0.797] | −0.026 [−0.115, +0.059] | +0.030 [−0.003, +0.068] |
| FOMC (16) | 0.703 [0.646, 0.774] | 0.644 [0.579, 0.716] | 0.816 [0.736, 0.884] | 0.800 [0.735, 0.865] | 0.692 [0.630, 0.762] | +0.060 [+0.022, +0.100] | +0.049 [+0.028, +0.073] |
| Gutenberg (7) | 0.656 [0.415, 0.884] | 0.563 [0.398, 0.738] | 0.744 [0.571, 0.916] | 0.773 [0.565, 0.956] | 0.736 [0.480, 0.912] | +0.094 [−0.214, +0.388] | +0.173 [−0.150, +0.425] |
| Domain-macro | 0.708 [0.666, 0.752] | 0.711 [0.679, 0.747] | 0.800 [0.766, 0.836] | 0.819 [0.780, 0.856] | 0.765 [0.719, 0.802] | −0.003 [−0.057, +0.045] | +0.053 [−0.0002, +0.095] |

Arm A is above arm B only on FOMC. Stack Exchange, CourtListener and web blogs
account for 589 of the 630 evaluation authors, and they dominate the pooled
endpoint. Weighting domains equally removes the A − B gap. The stylometric
control's gain over the encoder is also uneven: C − B is −0.001 on CourtListener
and on web blogs, and +0.082 [+0.066, +0.097] on Stack Exchange.

**By query length, k = 5.**

| Query words | A | B | C | D | E | A − B | E − B |
|---|---|---|---|---|---|---|---|
| 150–249 | 0.746 [0.733, 0.759] | 0.779 [0.766, 0.792] | 0.832 [0.821, 0.843] | 0.849 [0.839, 0.859] | 0.807 [0.796, 0.819] | −0.033 [−0.048, −0.018] | +0.029 [+0.022, +0.035] |
| 250–349 | 0.732 [0.711, 0.752] | 0.781 [0.762, 0.802] | 0.823 [0.804, 0.842] | 0.846 [0.828, 0.863] | 0.803 [0.784, 0.821] | −0.050 [−0.072, −0.027] | +0.021 [+0.012, +0.031] |
| 350–450 | 0.692 [0.609, 0.773] | 0.740 [0.665, 0.814] | 0.772 [0.681, 0.854] | 0.788 [0.702, 0.868] | 0.750 [0.667, 0.831] | −0.049 [−0.107, +0.005] | +0.010 [−0.014, +0.035] |

### T2, identification

Galleries hold 237 Stack Exchange, 162 CourtListener, 71 web-blog, 13 PLOS-blog,
11 FOMC and 5 Gutenberg authors. Results are pooled over domains at k = 5.

| Arm | Top-1 | MRR | Detection and identification at threshold | False accept on withheld authors | AUROC of max score, enrolled vs withheld |
|---|---|---|---|---|---|
| A, CFG | 0.112 [0.094, 0.130] | 0.206 [0.190, 0.225] | 0.013 [0.007, 0.019] | 0.115 [0.081, 0.149] | 0.556 [0.517, 0.593] |
| B, R0 encoder | 0.176 [0.156, 0.198] | 0.271 [0.251, 0.292] | 0.063 [0.049, 0.077] | 0.173 [0.131, 0.218] | 0.562 [0.520, 0.599] |
| C, stylometric control | 0.276 [0.250, 0.301] | 0.383 [0.360, 0.406] | 0.083 [0.066, 0.101] | 0.132 [0.100, 0.169] | 0.598 [0.555, 0.642] |
| D, fusion B + C | 0.341 [0.313, 0.370] | 0.449 [0.426, 0.474] | 0.098 [0.080, 0.117] | 0.130 [0.092, 0.167] | 0.616 [0.572, 0.659] |
| E, fusion A + B | 0.222 [0.199, 0.245] | 0.329 [0.307, 0.352] | 0.056 [0.043, 0.070] | 0.109 [0.075, 0.148] | 0.581 [0.537, 0.622] |
| A − B | −0.064 [−0.089, −0.040] | −0.065 [−0.088, −0.043] | −0.051 [−0.065, −0.037] | −0.059 [−0.115, −0.005] | −0.006 [−0.053, +0.041] |
| E − B | +0.045 [+0.026, +0.064] | +0.058 [+0.043, +0.073] | −0.007 [−0.019, +0.005] | −0.064 [−0.105, −0.024] | +0.020 [−0.004, +0.043] |

Top-1 rises with k for every arm. At k = 1 and k = 3 it is A 0.053 and 0.095,
B 0.081 and 0.148, C 0.112 and 0.214, D 0.156 and 0.287, and E 0.108 and 0.192.
The thresholds fitted on training authors mostly do not transfer. Across k, the
false-accept rate on withheld evaluation authors is 0.09 to 0.17, against the
0.10 target. At those thresholds no arm accepts as many as one in ten enrolled
queries with the correct identity.

### T3, author fit

| Arm | Primary mean AUC | Other-target negatives only | Full rewrites | Mirrors | Rewrite beats its own source (55 pairs, 52 targets) |
|---|---|---|---|---|---|
| A, CFG | 0.530 [0.498, 0.562] | 0.530 [0.497, 0.562] | 0.389 [0.309, 0.465] | 0.550 [0.517, 0.584] | 0.779 [0.663, 0.880] |
| B, R0 encoder | 0.539 [0.507, 0.572] | 0.539 [0.507, 0.571] | 0.286 [0.230, 0.348] | 0.584 [0.552, 0.618] | 0.474 [0.346, 0.610] |
| C, stylometric control | 0.545 [0.511, 0.577] | 0.544 [0.511, 0.576] | 0.311 [0.250, 0.374] | 0.582 [0.548, 0.616] | 0.846 [0.750, 0.932] |
| D, fusion B + C | 0.551 [0.518, 0.585] | 0.551 [0.518, 0.585] | 0.278 [0.222, 0.333] | 0.596 [0.563, 0.630] | 0.792 [0.679, 0.895] |
| E, fusion A + B | 0.546 [0.514, 0.578] | 0.546 [0.514, 0.578] | 0.292 [0.229, 0.355] | 0.587 [0.556, 0.620] | 0.636 [0.509, 0.763] |
| A − B | −0.009 [−0.047, +0.031] | | +0.103 [−0.003, +0.203] | −0.033 [−0.072, +0.007] | +0.304 [+0.134, +0.459] |
| E − B | +0.007 [−0.017, +0.031] | | +0.006 [−0.059, +0.070] | +0.003 [−0.022, +0.028] | +0.162 [+0.052, +0.275] |

Context for the primary endpoint: C − B is +0.006 [−0.028, +0.040] and D − B is
+0.012 [−0.016, +0.040]. Every arm is near chance, and only arm A's interval
includes 0.5. The kind and source columns restrict the positives only.

- **Record kind.** Kind moves the score more than the target does. For every
  arm, full rewrites of another writer's passage score below the negatives
  (0.28 to 0.39), and mirrors score above them (0.55 to 0.60).
- **Own source.** Under the target's profile, arms A and C score a rewrite above
  its own unrewritten source more often than arm B does (A − B +0.304 [+0.134,
  +0.459]). That is consistent with movement away from the source writer. The
  other-target column shows no reliable movement toward the particular target.
- **Generator family.** The primary mean AUC is 0.59 to 0.65 for one hosted
  family and 0.44 to 0.47 for the other, for every arm. A − B is not resolved in
  either family (−0.037 [−0.087, +0.015] and +0.029 [−0.028, +0.082]).
- **Train-split status.** For targets with other text in A Train, A − B is
  −0.025 [−0.081, +0.027]. For targets without, it is +0.009 [−0.052, +0.068].

**Secondary strata** (mean AUC; reported, not gated):

| Stratum | A | B | C | D | E | A − B | E − B |
|---|---|---|---|---|---|---|---|
| Hosted span rewrites | 0.519 [0.490, 0.549] | 0.514 [0.485, 0.542] | 0.502 [0.475, 0.531] | 0.502 [0.475, 0.530] | 0.520 [0.491, 0.551] | +0.005 [−0.034, +0.047] | +0.007 [−0.022, +0.036] |
| In R0's training bundle | 0.521 [0.500, 0.541] | 0.509 [0.490, 0.529] | 0.538 [0.517, 0.558] | 0.535 [0.514, 0.555] | 0.519 [0.498, 0.538] | +0.012 [−0.011, +0.034] | +0.010 [−0.005, +0.023] |
| Local Dev-iter | 0.519 [0.451, 0.588] | 0.500 [0.442, 0.561] | 0.531 [0.468, 0.598] | 0.526 [0.463, 0.593] | 0.510 [0.449, 0.574] | +0.019 [−0.063, +0.103] | +0.010 [−0.044, +0.064] |

The kind dependence recurs here. In the R0-trained stratum, A − B is +0.113
[+0.085, +0.141] for span rewrites and −0.148 [−0.180, −0.117] for mirrors.

### Coverage-restricted comparison

This comparison covers 293 evaluation authors and 780 queries, at k = 5. The
decision does not use it.

| Arm | Restricted text | All text, same authors | Coverage cost: primary minus restricted | Same-subset cost: all text minus restricted |
|---|---|---|---|---|
| A, CFG | 0.707 [0.688, 0.724] | 0.732 [0.713, 0.750] | +0.037 [+0.021, +0.052] | +0.025 [+0.016, +0.035] |
| B, R0 encoder | 0.747 [0.728, 0.766] | 0.777 [0.757, 0.796] | +0.030 [+0.014, +0.047] | +0.029 [+0.017, +0.041] |
| C, stylometric control | 0.795 [0.776, 0.813] | 0.830 [0.813, 0.846] | +0.033 [+0.016, +0.050] | +0.035 [+0.026, +0.044] |
| D, fusion B + C | 0.813 [0.796, 0.831] | 0.849 [0.833, 0.865] | +0.033 [+0.018, +0.048] | +0.036 [+0.026, +0.045] |
| E, fusion A + B | 0.769 [0.751, 0.787] | 0.803 [0.785, 0.821] | +0.036 [+0.019, +0.051] | +0.034 [+0.023, +0.043] |

On restricted text, A − B is −0.041 [−0.060, −0.020] and E − B is +0.022
[+0.014, +0.029]. With all text for the same authors, A − B is −0.045 [−0.066,
−0.024] and E − B is +0.026 [+0.018, +0.034]. Restricting this subset to
completely parsed text moves A − B by +0.0039 [−0.0094, +0.0175] (computed by a
verifier with the same bootstrap stream). That is a small,
unresolved narrowing, and arm A still stays below arm B. Arm A also has the
smallest same-subset cost of the three single arms. Against the full primary
population, the gap is wider on restricted text (−0.041 against −0.034), but
those two populations differ.

## Decision-rule outcome

| Rule | Contrast | Point | 95% interval | Result |
|---|---|---|---|---|
| 1, CFG wins clearly | A − B, T1 | −0.034 | [−0.047, −0.022] | below zero |
| | A − B, T3 | −0.009 | [−0.047, +0.031] | includes zero |
| | | | | **not met** |
| 2, CFG adds value | E − B, T1 | +0.027 | [+0.022, +0.032]; 97.5% [+0.021, +0.033] | above zero |
| | E − B, T3 | +0.007 | [−0.017, +0.031] | includes zero |
| | | | | **met through T1** |

There are 249 primary T3 targets, at least the 30 required, so rule 1 was
available. The outcome is **CFG adds value**. As a point difference, which is not
a pre-specified contrast, arm E's primary AUC is 0.042 below arm D's.

## Verification

Two verifiers worked independently, on build-host only and with aggregates only.
Both found that the outcome holds.

**What they reproduced.**

- **Metrics.** One verifier recomputed, in its own Rust code, every reported
  point estimate from the saved pair files to full precision. The pair files'
  hashes match the execution record.
- **Intervals.** It reimplemented the bootstrap from the protocol text, and that
  reproduced every checked interval and SE exactly.
- **Scores from features.** It rebuilt arm A from the CFG counts, arm B from
  `h_raw`, and arm E through out-of-fold training refits. The training CV means,
  fusion weights and normalization constants match. The evaluation scores match
  to under 4e-14 across 1.25M T1/T2 and 109k T3 pair scores. This confirms that
  every statistic, fusion weight and threshold was fitted on training authors
  only.
- **Structure.** It confirmed the impostor rule, one genuine row per query,
  withheld authors never enrolled, the T3 labels and exclusions, and the author
  roles.
- **Integrity.** The other verifier confirmed the integrity hashes, the
  timeline, the role and leakage checks, and the absence of any use of a reserved
  split. It also ran a private-text audit with a positive control. None of 36,377
  distinct 8-word windows from the run's tracked files, logs and records occurs
  in any of the 22,873 input texts.

**Findings.**

1. **Pooling departs from the frozen text, and the departure was not logged.**
   The evaluator unit-normalizes each standardized reference before averaging.
   `FROZEN.json` says to normalize the mean of the standardized references, which
   is also how the existing contract of the same name is written. For a single
   arm, the two readings agree at k = 1 and differ at k = 3 and 5 and on T3. A
   fused arm differs at every k, through its k = 5 fusion weights. The private
   deviation log is unchanged, so its recorded digest still matches the evaluator
   and execution records. This note records the finding.

   After the results were known, one verifier evaluated arms A, B and E again
   under the frozen wording. This is a sensitivity check, not a second governed
   evaluation. It repeated the selection CV, the fusion refit and the evaluation,
   with the same bootstrap stream:

   | Under the frozen wording | Value |
   |---|---|
   | Training CV selection | unchanged: arm A pool-merged leaves S1 (0.752), arm B `h_raw` S1 (0.791) |
   | T1 AUC, k = 5 | A 0.741 [0.730, 0.752], B 0.776 [0.764, 0.787], E 0.803 [0.794, 0.813] |
   | A − B | T1 −0.034 [−0.047, −0.022]; T3 −0.012 [−0.050, +0.029] |
   | E − B | T1 +0.028 [+0.022, +0.033], 97.5% [+0.022, +0.034]; T3 +0.005 [−0.019, +0.030] |
   | Outcome | **CFG adds value** (unchanged) |

   Not rescored under that wording: arm A's non-selected candidates, arms C and D,
   T2, and the coverage-restricted comparison. The non-selected arm A candidates
   were at least 0.009 behind under the pooling as run. A training-only rehearsal
   by the other verifier gave mean CV AUC of A 0.752, B 0.791 and C 0.836 under the
   frozen wording, against 0.754, 0.795 and 0.836 as run.
2. **Rounding.** The protocol note's evaluation section had four values off by
   0.001, through double rounding: arm C's AUC (0.828), arm E's lower interval
   bound (0.795), arm A's T2 MRR (0.206) and arm B's detection-and-identification
   rate (0.063). This note rounds once from full precision, and the protocol note
   now points here. None of these values affects the decision.
3. **Exposure.** The protocol note understated each arm's indirect exposure. The
   table above corrects it.
4. **Coverage wording.** The protocol note said that restriction "does not
   narrow" the A − B gap. The accurate statement is in the coverage-restricted
   section above.
5. **Operational.** The R0 feature shards ran without the big-memory lock while
   cgroup memory including page cache reached about 6.5 GB (anonymous memory about
   5.1 GB). The deviation log records this. The outputs are deterministic and
   unaffected.
6. **Timestamps.** `FROZEN.json`'s modification time is about 2 minutes after its
   own recorded freeze time. It is still before the hash file, the freeze commit
   and the first feature job.

**Training-only sensitivity of arm A's scaling.** These runs scored no
evaluation author. With frozen settings, arm A's best CV AUC is 0.754. Lowering
the SD floor to 1e-9 gives 0.717, and the stylometric arm's `log1p(1000 × rate)`
scaling gives 0.746. So the floor, which binds on 740 of 756 coordinates, did not
handicap CFG. Arm A stays below arm B (0.795) under every scaling tried.

## Follow-up: CFG on top of encoder plus stylometry

Protocol `cfg-vs-encoder-20261031` version 2 asks one question that v1 left
open: does CFG add anything once both the encoder and the stylometric control
are present? Arm F is the score fusion of arm B (encoder), arm C (stylometric
control) and arm A (CFG). Its single contrast is F − D on the v1 primary
endpoint. Version 2 was written and frozen after the v1 evaluation and its
verification, and before any arm-F quantity was computed. A verifier checked
the run, found that the result holds and reported no problems.

### Re-used evaluation authors

**The follow-up scores the v1 evaluation authors a second time.** It uses the
same 630 evaluation authors (1,877 queries) and the same 249 primary T3 targets
(438 records). They are not fresh:

- v1's per-pair scores and metrics for arms A to E, including arm D, had been
  read.
- After the v1 results were known, a verifier had rescored arms A, B and E on
  them under the frozen pooling wording (finding 1).
- The contrast was chosen after reading v1, when arm D was the best arm and the
  fusion of B, C and A was named as untested.

F − D uses the same authors, queries, impostor sets and bootstrap replicates as
v1, so its interval is correlated with v1's intervals. It is not an independent
replication and cannot be combined with v1 as separate evidence. The family of
tests run on these authors grows by one, and no correction is applied across
protocol versions. The result describes these authors only. A replication would
need fresh authors under a separate protocol. v1's caps left most eligible
evaluation-role Stack Exchange and CourtListener authors unused, but their
features were never extracted.

### Method

- **Unchanged from v1.** The populations; the extracted features, with nothing
  re-extracted; arms A, B and C at v1's selected configurations (pool-merged
  grammatical leaves, `h_raw`, and function words plus character 3-grams, all
  S1); the impostor-normalization constants; the pooling as run; and the
  bootstrap stream and its draw order. Every replicate has v1's multiplicities.
- **Arm F.** Each component's cosine at k is z-normalized with v1's constants,
  and F = b0 + b_B z_B + b_C z_C + b_A z_A. The weights come from a
  class-balanced logistic regression with a 1e-3 penalty on the three slopes,
  fitted by Newton's method on the same 43,774 out-of-fold training T1 trials at
  k = 5 (1,412 genuine) on which v1 fitted arms D and E. One set of weights
  serves every k and task. F has no candidates to choose among, and no
  evaluation-author score enters any fitted quantity.
- **Decision rule**, frozen before any arm-F quantity was computed. CFG adds
  resolved value on top of the encoder and stylometry only if the 95% percentile
  interval of Δ = AUC_F − AUC_D (T1, pooled, author-weighted, k = 5) lies above
  zero, with its lower bound strictly greater than 0. There is one primary test
  and no margin. The 97.5% interval is computed but not used. T3 is not
  co-primary: in v1 every arm was near chance on T3, and D − B there was
  unresolved. Neither outcome changes the v1 outcome.

**Execution.** Times are UTC on November 1, 2026 (the evening of October 31,
local time). `FROZEN-v2.json` (SHA-256
`522b03633411a3f92a6eb507d2793d8926e7fea4da9352f13b0a5583469074b8`) was frozen at
00:26:45. The evaluator commit `f3ed1561` followed at 00:42. It embeds that hash
and keeps v1's pooling, statistics, task and loading code byte-identical. The
evaluator binary (`8bfaf740…`) and the version 2 clarification log were recorded
at 00:43, before any arm-F quantity was computed. One rehearsal on training
authors only followed. The training selection, with F's weights, was recorded at
00:45, before any evaluation author was scored. The single evaluation ran from
00:45 to 00:48, including the row-by-row pair check. It used 4 threads, a peak
of 1.8 GB RSS and 2 min 8 s of evaluation wall time. There were no failed
attempts, no execution-failure exclusions and no recorded deviations. A first
build failed one new synthetic test, whose expected value used a different
floating-point order from the code; the test was corrected before the commit.
No step used Dev-select, CAL or the consumed Test split. No step made a
hosted-model, external detector or Pangram call.

**Reproduction gate.** The evaluator writes no arm-F output unless it reproduces
v1 exactly. Both checks passed:

- **Training phase.** The recomputed v1 selection record has v1's digest
  (`e69f0144…`) and is JSON-equal to it, with no differing field.
- **Evaluation.** The recomputed v1 analysis of arms A to E is JSON-equal to
  v1's results file (`a5448564…`), with zero differing pointers. That analysis
  covers every metric, contrast, population, pair-file digest and the decision.
  After the run, every v1-arm score in the six version 2 pair files (1,657,470
  rows) matched v1's row by row. v1's rule, recomputed, still gives **CFG adds
  value**.

Arm D here is therefore exactly v1's arm D, and F − D is paired with it.

### Follow-up results

Values in this section have four decimals, or five for T1 pooled F − D, rounded
once from full precision.

**Primary endpoint.**

| Quantity | Value |
|---|---|
| Δ = AUC_F − AUC_D, T1 pooled author-weighted AUC at k = 5 | **+0.00146** |
| 95% percentile interval | [+0.00035, +0.00253], above zero |
| Bootstrap SE; 1.96 × SE | 0.00058; 0.00113 |
| Valid replicates | 2,000 of 2,000 |
| 97.5% interval (computed, not used) | [+0.00017, +0.00272] |
| Outcome under the version 2 rule | **CFG adds resolved value on top of the encoder and stylometry** |

**Fusion weights.** These were fitted on training authors only. The frozen
bootstrap resamples evaluation authors with every fitted quantity fixed, so the
weights have no interval.

| Fusion | Trials (genuine) | b0 | Encoder (B) | Stylometric control (C) | CFG (A) |
|---|---|---|---|---|---|
| F, all text | 43,774 (1,412) | −1.193 | +0.754 | +0.927 | +0.217 |
| F, coverage-restricted refit | 7,818 (598) | −0.843 | +0.569 | +0.835 | +0.128 |
| D (v1), all text | 43,774 (1,412) | −1.169 | +0.793 | +1.014 | |
| E (v1), all text | 43,774 (1,412) | −0.880 | +0.992 | | +0.657 |

Both F fits converged in 7 Newton iterations. With the stylometric control
present, CFG's weight is about a third of its weight in arm E.

**Pre-specified descriptive outputs.** These have no decision role and are not
called resolved.

| Quantity | F | D | F − D |
|---|---|---|---|
| T1 pooled AUC, k = 1 | 0.7625 [0.7504, 0.7742] | 0.7608 [0.7488, 0.7725] | +0.00162 [−0.00003, +0.00324] |
| T1 pooled AUC, k = 3 | 0.8276 [0.8182, 0.8376] | 0.8262 [0.8171, 0.8362] | +0.00131 [+0.00006, +0.00253] |
| T1 pooled AUC, k = 5 (primary) | 0.8479 [0.8395, 0.8568] | 0.8464 [0.8380, 0.8556] | +0.00146 [+0.00035, +0.00253] |
| T1 domain-macro AUC, k = 5 | 0.8206 [0.7854, 0.8550] | 0.8194 [0.7802, 0.8559] | +0.0012 [−0.0055, +0.0114] |
| T3 primary mean AUC | 0.5502 [0.5168, 0.5826] | 0.5511 [0.5178, 0.5847] | −0.0010 [−0.0056, +0.0037] |

The bootstrap SE of F − D is 0.00083 at k = 1, 0.00063 at k = 3, 0.0041 for the
domain-macro AUC and 0.0023 on T3.

| Domain (evaluation authors), T1 AUC at k = 5 | F | D | F − D |
|---|---|---|---|
| Stack Exchange (300) | 0.9386 [0.9307, 0.9466] | 0.9383 [0.9303, 0.9465] | +0.0003 [−0.0009, +0.0015] |
| CourtListener (200) | 0.7345 [0.7139, 0.7553] | 0.7314 [0.7105, 0.7525] | +0.0031 [+0.0005, +0.0058] |
| Web blogs (89) | 0.8727 [0.8459, 0.8998] | 0.8717 [0.8445, 0.8989] | +0.0010 [−0.0021, +0.0039] |
| PLOS blogs (18) | 0.7987 [0.7211, 0.8709] | 0.8018 [0.7264, 0.8720] | −0.0032 [−0.0107, +0.0048] |
| FOMC (16) | 0.7978 [0.7346, 0.8617] | 0.8004 [0.7348, 0.8647] | −0.0026 [−0.0094, +0.0048] |
| Gutenberg (7) | 0.7815 [0.5986, 0.9330] | 0.7730 [0.5645, 0.9558] | +0.0085 [−0.0306, +0.0680] |

**Other outputs, not pre-specified.** The shared code also computes these. They
are descriptive only.

- **T1 error rates at k = 5.** F − D is −0.0018 [−0.0058, +0.0024] in EER (F
  0.2316) and −0.0022 [−0.0155, +0.0104] in FPR at TPR 0.90 (F 0.4173).
- **T2 at k = 5.** F's top-1 is 0.3514 [0.3236, 0.3800] and its MRR 0.4579
  [0.4345, 0.4830]. F − D is +0.0100 [+0.0007, +0.0193] and +0.0087 [+0.0029,
  +0.0143]. On withheld authors, F's false-accept rate is 0.1272 [0.0917,
  0.1644] against the 0.10 target, and F − D is −0.0025 [−0.0177, +0.0123].
- **Coverage-restricted comparison** (293 authors, k = 5). On completely parsed
  text, F's AUC is 0.8135 [0.7959, 0.8314], and F − D is +0.0001 [−0.0011,
  +0.0015]. With all text for the same authors, F − D is +0.0005 [−0.0013,
  +0.0023].

**Reading.** The gain is resolved but very small. It is about one-eighteenth of
E − B (+0.027) and about 2% of D − B (+0.069). It is also much smaller than the
width of either arm's own AUC interval, and it is resolved only because the
contrast is paired. Descriptively, the domain-macro contrast includes zero. Only
CourtListener's per-domain interval lies above zero. Author fit (T3) shows no
gain, and neither does completely parsed text on the restricted subset. As the
protocol states, a positive result means this: on these authors, with unlearned
cosine profiles and this linear score fusion, adding CFG to the encoder and the
stylometric control raises pooled verification AUC by a resolved amount. It does
not show that CFG writer vectors qualify for component C or for a named
`provided_writer` match.

### Pooling addendum (summary)

A private addendum to protocol v1 records the pooling departure of finding 1. It
was written after the v1 evaluation, the reading of its results and both
verifiers' reports. It changes no frozen rule, governed number or outcome. v1's
deviation log stays as it was at evaluation time, so its digest still matches
the v1 evaluator and execution records.

- **Frozen wording.** The profile direction is the sum of the standardized
  references, z_1 + … + z_k. A reference with a larger standardized norm pulls
  the profile further.
- **As run.** The profile direction is z_1/‖z_1‖ + … + z_k/‖z_k‖, and a query is
  scored by cosine with it. Every reference has equal weight in direction. The
  addendum names this pooling
  `cfg_vs_encoder_v1_as_run_reference_unit_mean_cosine`. v1's numbers are cited
  under that name, not under the contract
  `slop_ninja_reference_normalized_mean_cosine_v1` as worded.
- **What it affects.** Single arms are identical under both readings at k = 1
  and differ at k = 3 and 5. That covers the T1 primary endpoint, T2 at k = 3
  and 5, every T3 stratum (T3 uses k = 5 profiles), and the coverage-restricted
  comparison at k = 3 and 5. It also covers the k = 5 training CV means that
  drove selection and the k = 3 and 5 impostor constants. The fused arms D and E
  differ at every k, including k = 1, through their k = 5 fusion weights, and so
  do their T2 thresholds. This note earlier said that the departure changes no
  k = 1 score. That holds for single arms only, and the methods paragraph and
  finding 1 now say so.
- **Sensitivity.** The verifiers' checks are those in finding 1. Under the
  frozen wording, the selection and the v1 outcome are unchanged. Arms C and D
  were not rescored under it, so their values under the frozen wording are
  unknown.
- **Version 2** keeps the as-run pooling, so that its arm D is exactly v1's arm
  D. F − D is evaluated under that pooling only. The frozen wording is not run
  in version 2.

### Limits of the follow-up

- **Re-used authors** (see above). The result is not independent of v1 and
  describes these authors only.
- **v1's limits carry over.** The matcher is an unlearned cosine. Topic
  continues within an author. Stack Exchange, CourtListener and web blogs
  dominate the pooled endpoint. The web-blog host label is a weak author label.
- **One fusion form.** Only a linear logistic combination of impostor-normalized
  scores was tested, with the same weights at every k. Neither a learned matcher
  nor a feature-level combination was tested.
- **One pooling.** F − D was evaluated under the as-run pooling only.

## Limits

- **Unlearned matcher.** Every arm uses an unlearned cosine on a pooled profile.
  The design feeds the writer vector to a learned matcher, and this test does not
  measure that path for any arm.
- **Arm A's projection** was built for a supervised detector, not for
  authorship. A projection tuned for authorship would need a new frozen protocol.
- **Arm B** uses R0's head representation, because the pre-head hidden-state
  mean is not exposed. R0 is one detector seed and was not trained for
  authorship.
- **Topic.** T1 rewards topic continuity within an author. Queries and
  references come from different source groups but often share topics. The
  character 3-grams and the encoder carry lexical and topic signal, while CFG
  excludes word identity by design. No topic-controlled stratum exists.
- **Endpoint mix.** Three domains dominate the pooled endpoint. Domain-macro
  A − B is −0.003 [−0.057, +0.045]. The web-blog label is a host, which is a weak
  author label. Gutenberg has 7 evaluation authors.
- **T3.** T3 is near chance for every arm, with interval half-widths of 0.03 to
  0.04, and record kind confounds it. Its voices come from single Gutenberg
  passages, not the five-post route. Rule 1 could therefore hardly pass through
  T3. This does not change the outcome, because A − B on T1 is already negative.
- **Exposure** to same-domain training data is larger for arm B.
- **CFG beyond the encoder plus stylometry** was tested only by the
  follow-up, on re-used evaluation authors and with one linear fusion. Its gain
  is resolved but very small (+0.00146 AUC). "Adds value" is a weak bar: CFG's
  resolved gain over the encoder alone is about half the gain from the
  stylometric control alone.
- **T2 operating points.** Thresholds fitted on training authors mostly overshoot
  the false-accept target on withheld evaluation authors.
- **Scope.** Six public domains, queries of 150 to 450 words, and at most five
  references per profile. The governed run used the pooling departure described
  above.

## What the evidence bears on

These are readings of the evidence, not design changes. The whitepaper and the
project conventions are unchanged.

- **Component C (shared CFG contributions).** This test finds no evidence that
  CFG writer vectors are a better writer-profile basis than encoder profiles.
  CFG's resolved contributions are verification gains in score fusions. Fused
  with the encoder, it gains less than function words and character 3-grams do.
  Added to the encoder plus stylometry, it gains +0.00146 AUC on re-used
  evaluation authors, which is resolved but very small. On writer-profile
  evidence alone, these results give no support for keeping CFG as the profile
  basis. Any case for keeping component C has to rest on other evidence or on
  further tests.
- **Writer profiles and `provided_writer`.** Encoder profiles beat CFG profiles
  at every k and on identification. Encoder plus stylometry (arm D) is the
  strongest v1 profile, and adding CFG (arm F) raises its primary AUC by only
  0.0015. Absolute operating points are weak for every arm, including arm F. At
  k = 5, 42% to 64% of impostor trials pass when 90% of genuine trials do, and no
  arm's training threshold holds its false-accept target on withheld writers. No
  unlearned profile tested here would qualify a named `provided_writer` match at a
  useful operating point. That would require a learned matcher and thresholds
  measured on withheld writers.
- **B author-fit feedback.** No tested profile reliably ranks rewrites toward a
  target above rewrites toward other targets. Record kind moves these scores more
  than the target voice does. None of the profiles here is therefore a validated
  author-fit signal for iterative revision. A direct test still requires
  rewrites conditioned on several of the target's own samples, and that data
  does not exist yet.
