# Shared A: frozen final Test

Measured on build-host, October 26, 2026. The bounded training experiment has
finished. The selected shared model remains short of Pangram parity, with a
substantial human false-positive problem on the reserved population. This Test
is consumed: its results cannot select checkpoints, calibration, features or
thresholds for a subsequent experiment.

## Detection and CFG comparison

The [aggregate evidence](a-final-test-results.json) contains all sixteen
configurations, every seed, calibration bins and family-aware intervals. Brier
below gives equal weight to source families and to observed origin classes
within each family. This is the frozen research population, not deployment
prevalence. FPR uses the diagnostic 0.5 involvement cutoff; the API does not
require a boolean output. Share error uses equal-family weighting and measured
replacement-span labels.

| Candidate | Involvement Brier ↓ | Human FPR ↓ | Share MAE, points ↓ |
| --- | ---: | ---: | ---: |
| Frozen V6, original calibration | 0.12434 | 12.50% | Not provided |
| **Preselected warm text** | **0.07661** | **23.96%** | **9.04** |
| Matched warm text+CFG | 0.07841 | 21.88% | 9.42 |
| Full-data text, 333 Train works | 0.05937 | 12.50% | 9.13 |
| Full-data text+CFG | 0.06625 | 14.58% | 9.68 |
| Grammatical-leaf CFG only | 0.14692 | 43.75% | 21.30 |
| All aggregate CFG | 0.15834 | 51.04% | 22.63 |
| Four-bin ordered CFG probe | 0.17452 | 70.83% | 27.99 |
| Matched global CFG control | 0.14569 | 41.67% | 21.53 |
| CAL-mapped Pangram comparator | 0.01537 | 0 observed | Different output meaning |

The selected-minus-Pangram Brier gap is **+0.06124**, with paired 95% family
interval **[+0.02852, +0.10131]**. Selected-minus-V6 is −0.04773
[−0.09590, +0.00578]: the observed reduction is substantial but its interval
includes zero. The selected model flags 7–8 of 32 human excerpts per seed,
versus four for V6 and zero for Pangram. These excerpts come from sixteen works;
neither zero flags nor these bootstrap intervals establish a rare-FPR guarantee.

Mean selected three-way balanced accuracy is 88.59%, three-way Brier sum 0.16764,
and involvement ECE10 0.06709. With natural equal-family frequencies, involvement
Brier is 0.05451 versus 0.07251 for the fitted Train-family prior; the corresponding
three-way accuracy is 92.11%. Those differently weighted figures must not replace
the primary balanced comparison. Frozen V6's pure human/model Test Brier is
0.02822; its mixed-only Brier is 0.31659. Its historical 95.1% binary accuracy
and 0.03264 Brier are separate evidence, not results on this population.

There is no consistent CFG benefit. Warm combined-minus-text Brier is +0.00180
[−0.00770, +0.01017]. The earlier small MLP's combined-minus-text result is
−0.01005 [−0.02285, +0.00182], an uncertain gain. Full-data CFG fusion worsens
Brier by +0.00687 [+0.00291, +0.01119]. The four-bin probe is worse than its
global control by +0.02883 [+0.01253, +0.04656]. These are fixed descriptive
contrasts, without a multiple-comparison guarantee. They do not test full tree
topology or sentence-level learned representations.

Full-data text scores better than the selected model by 0.01724 Brier on Test
(selected-minus-full-data interval [+0.00060, +0.03417]), although it ranked
worse on Development. This reversal supports larger, more representative
selection populations; it cannot authorize reselection on this Test.
Uncertainty resamples whole source families after averaging differences over
the three fixed seeds. It holds fitted models and calibration fixed and does
not include training-data or calibration-estimation uncertainty.

## Share and attribution

The selected model's share MAE is 9.04 points, MSE 164.57 squared points and bias
−0.02 points. Near-zero overall bias hides errors of opposite signs:

| Realized share | Documents | Share MAE, points | Bias, points |
| --- | ---: | ---: | ---: |
| 0% | 32 | 5.74 | +5.74 |
| (0,25%] | 86 | 13.02 | +1.32 |
| (25,50%] | 20 | 14.36 | −0.34 |
| (50,75%] | 86 | 13.71 | +8.59 |
| (75,100%) | 0 | Unavailable | Unavailable |
| 100% | 190 | 5.37 | −5.37 |

The 100% endpoint includes 94 model-only drafts and 96 full human-source
rewrites. No intermediate high-share generalization follows from those endpoints.
Pangram's raw content fraction has 4.81-point diagnostic MAE against this span
convention. Its fraction supplies neither share ground truth nor our any-AI
confidence. The involvement comparison uses only the frozen CAL isotonic
mapping; no Test recalibration occurred.

Known-human conditional head top-1 averages 20.56% across six supported writers
(240 role-present queries); known-generator top-1 is 77.65% across two supported
identities (254 queries). The calibrated heads fall back on **95.69% of known
human roles** and **75.49% of known generator roles**. Withheld rejection is
98.33% for eighty human-role queries and 82.55% for 128 generator-role queries.
Frequent rejection of known profiles prevents interpreting high withheld
rejection as successful attribution.

Independent-reference matching uses the shared document encoding, mean pooling
and cosine matching. Human top-1 is 12.60% in an eight-writer gallery, with MRR
0.36575; known-writer top-1 is 14.31% and withheld-writer top-1 7.50%. This remains
a poor retrieval diagnostic, not a qualified customer-profile mechanism.
There is no separate generator reference gallery; generator retrieval here is
conditional head ranking. Missing gallery metrics remain unavailable.

Matched origin-only and attribution-only controls were evaluated on Development
under the same architecture, data and budgets. Joint training helped generator
ranking there; it did not establish a general mutual benefit for detection and
writer attribution. Those [control results](a-balanced-baseline.md#selected-residual-single-task-controls)
remain Development evidence; the frozen final-Test roster contains joint
candidates only. Every documented role is scored without an involvement cutoff;
absent roles' conditional identities are not treated as asserted contributors.

## Population and frozen comparison

Development selected `warm_text` before any final-Test acquisition. Sixteen
joint configurations, each with seeds 17, 29 and 41, were frozen together.
Seed 17 is the predeclared representative; tables average all three seeds,
not the best seed. Other candidates remain descriptive comparisons and cannot
replace that selection using Test results.

The training pool grew to 3,629 queries from 333 source works. The selected
residual was trained on the original 215 queries from twelve works. Expanded
training changed domain mix as well as sample count. Under the fixed mixture,
the original works retain half the loss mass; weight concentration corresponds
to 45.84 equally weighted families, not 333 equally influential works or an
effective independent sample size. Learning curves show later overfitting.
[Development comparisons](a-balanced-baseline.md) retain the matched controls,
source-size experiment and selection evidence.

Final Test contains 414 accepted queries from sixteen reserved works, plus
four separately parsed references from two other works. Of 416 requested
queries, two from-scratch generations failed after bounded attempts. All 414
assembled queries are supported by every candidate and frozen V6, without
truncation; every accepted query has a successful retained Pangram observation.
The query histories are 32 historical human proxies, 94 model-only drafts and
288 mixed rewrites. The Test-only generator and two withheld writers differ
from their Calibration counterparts. The human reference gallery has sixteen
excerpts from eight independent works for eight writers, including the two
Test-withheld writers; this does not expand classification heads.

Human sources are historical public-domain fiction proxies, not authenticated
contemporary human-only workflows. Exact source ancestry, licenses, generation
versions, attempts, replacement bytes, final spans and word counts are retained.
Share is the realized final-word fraction inside generated replacement spans.
Full rewrites retain mixed production history even when that fraction is one.
Sham reassembly checks preserve exact source bytes. These controls do not prove
that splice cues, edition artifacts or pretraining exposure have been removed.

The cumulative audit checked protected IDs, source families, aliases, text
hashes and original scratch prompts before acquisition. Historical V6 Test
overlap was checked through retained source metadata; complete historical
normalized Test hashes were unavailable. The grammar, fallback and resolver
were frozen. No Test-driven grammar changes or fitting occurred.

## Inputs, outputs and inference

The selected model uses frozen V6's 768 classifier-input coordinates and a
learned shared residual encoder. A 2,588-coordinate input comprises those text
coordinates and a constant-zero 1,820-coordinate CFG block. The residual branch
is `2588 → 64 tanh → 768`; its output is added to the raw V6 representation.
Origin, conditional mixed share, human attribution and model attribution all
consume the same resulting encoding. The combined control instead supplies
910 grammatical-leaf rates and 910 availability coordinates in the CFG block.
That representation preserves neither full tree topology nor sentence order.

The numeric contract returns all three origin probabilities, both attribution
roles and their involvement probabilities. Confidence is
`100 * (P(model_only) + P(mixed))`; share is
`100 * (P(model_only) + P(mixed) * sigmoid(mixed_share_logit))`.
This enforces the required probability/share bounds. Conditional fallbacks do
not assert role presence. Unavailable supervision was masked during training;
documented roles are evaluated even when predicted involvement is low.

All fitted artifacts, checkpoints, numeric weights, reference predictions,
normalization and immutable selection receipts are retained in ignored storage.
The [inference entry point](../inference/a-v6-cpu/README.md) runs exact text
through V6 and the selected shared head in native Rust on Linux x86-64.
[Its qualification](../inference/a-v6-cpu/QUALIFICATION.md) passes 41 synthetic
cases and bundled V6 references, with no Python, libtorch or CUDA runtime.
Weights total approximately 605 MB; peak process RSS is 1.67 GiB, and a
2,048-token encoder forward takes about thirteen seconds on one CPU. These
measurements do not qualify Workers/Wasm or Cloudflare Containers. No deployment
was implemented.

First-time final-Test CFG extraction took 15m27s for 414 documents, with 434,536
KiB peak RSS, 5.44 seconds of parser loading, and document p50/p95 of 1.95/4.34s.
V6 extraction took 12m28s and 1,132,560 KiB peak RSS. These separate capped jobs
used one CPU each and zero swap. All 606,553 bytes are accounted for. Of 125,631
tokens, 118 remain unresolved; 62 of 6,455 windows reached resource limits.
Native-complete, fallback-complete and partial windows remain distinguished.
No document was dropped for incomplete parsing. Cached projection and sub-ms
numeric heads do not eliminate first-time encoder or parser work.

## Evidence and reproduction

Selection receipt SHA-256:
`eeaebddd81b29b9bf18f10f4983b37b00768db27b4c07bc54f8107577eda71b7`.
Dataset manifest SHA-256:
`cbd428c26a3339726994d453cc9f677ee333a4b899b3c40aaa8d379efcc66a93`.
Dataset records SHA-256:
`f46ebfc4c751ce13d67ee5ba3c0e223ace7eb5ecfbbe85b57ba635d6fb6d3ce3`.

Detailed evidence lives under the existing ignored A task root's
`train-scale/large-batch-v1/final-evaluation-v1/`. It retains acquisition and
overlap receipts, generation attempts, exact span labels, frozen projections,
all models' predictions, reference galleries, resource logs and checksums.
Its `ARTIFACTS.json` pins the representative checkpoint, numeric weights,
preprocessing contract, native inference binary and reference predictions.
All 414 new Pangram calls have permanent request/intent/submission/raw-response
fixtures; archived Dev/CAL observations and the CAL-only mapping are unchanged.
Pangram returned version 4.1. No paid retries or quota failures occurred.

The [Rust evaluation interfaces](../grammar/crates/grammar-a-baseline/README.md)
take the frozen numeric bundle, fit directory and selection receipt:

```text
predict_final_test NUMERIC FIT SELECTION NEW_PREDICTIONS
evaluate_supervised --frozen-test NUMERIC FIT RECEIPT SELECTION NEW_REPORT GALLERY
compare_supervised_pangram --frozen-test NUMERIC FIT RECEIPT PANGRAM_FIXTURES SELECTION NEW_REPORT
evaluate_frozen_v6 --frozen-test NUMERIC TEST_EMBEDDINGS FROZEN_EMBEDDING_MANIFEST SELECTION NEW_REPORT
summarize_final_test PLAN.json NEW_PUBLIC_JSON
```

The recorded `evaluation-jobs-v1/run.sh` and `summary-jobs-v1/run.sh` pin the
actual inputs and retained executables. Use new output paths for a replay.
No replay requires generation, training, or additional paid API calls.
Campaign checkpoints and caches remain preserved and paused.

The reference/projection routes passed 241 focused offline tests, and the final
aggregate helper passed 46 tests. All 48 frozen predictions and metric reports
completed, the aggregate validator rechecked their bindings and recalculated
comparisons, and the whitepaper was rebuilt on build-host. Tests made no live
provider calls. Raw text, fitted profiles and provider exchanges remain outside
Git; the public report contains counts, metrics and cryptographic bindings only.

## Conclusion and next experiment

The evidence supports continuing text-based shared A research. It does not
support a CFG-only launch, Pangram parity, reliable writer attribution or a
Cloudflare deployment. More epochs on the existing head are not justified by
the learning curves. More independent data helped within the fixed source-size
comparison, but additional rewrites of a few works cannot replace source and
genre diversity or establish absence of overfitting.

The next bounded experiment should enlarge source-balanced human Train,
Development and Calibration populations, preserve established authors and
independent reference works, and cover missing share ranges with exact spans.
Reserve a new Test before fitting. Compare the frozen V6 encoder against one
low-learning-rate adaptation of its final block, retaining the same shared heads,
fixed budgets and repeated seeds. That tests whether the frozen representation
limits detection while maintaining the original-model control. Keep this Test
as consumed regression evidence. A separate sentence/tree CFG experiment is
still warranted if grammar-only inputs remain a priority; these count probes
do not settle that hypothesis. Sentence and paragraph confidence, natural edit
histories, modern documented human workflows and serverless execution remain
separate qualifications.
