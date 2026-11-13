# Pangram dataset collection within a fixed budget

Declared 2026-09-12 before the first submission under the amended budget.
The collection works within a fixed budget, which applies cumulatively with the
two earlier jobs, and aims for varied AI percentages, preferably spanning
60-100%. The budget and every charge estimate are recorded in the private
ledger. Each new reservation must cover at least
the realtime-price estimate, above the discounted bulk estimate. The earlier,
larger proposal will not be submitted as a whole.

## Measure the requested range

Use Pangram 4's combined AI and AI-assisted content fraction for continuity
with the existing comparison. Also retain the separate AI, AI-assisted and
human fractions and every segment-level result. These fractions describe
detected content, not a calibrated probability of document authorship.

Report bins below 60%, 60-70%, 70-80%, 80-90%, 90% to below 100%, and 100%.
For binning only, treat combined sums within 0.00001 of one as 100%, matching
the collector's floating-point tolerance. Preserve the original numbers.
Aim for coverage across the interior bins,
without discarding observations outside the requested range. Report counts
separately for model-only writing, mixed workflows and human controls.

The cached 57-text Development comparison motivates a pilot: five of its
19 mixed-origin examples have intermediate combined fractions, and two
model-only drafts also score well below 100%. Their independently recorded
origin labels remain unchanged. Most other nonhuman examples score 100%.
These are prior observations, not results from this new Train collection.

## Start with complete revision families

Use the closed Train revision export from v6: 454 complete families, each
containing a historical human proxy, an original model draft, a direct edit
of the human root, a Qwen Anti-AI revision of the draft and a subsequent
Mistral Fix Slop revision. All five records retain their exact text and
provenance. The input SHA256 is
`06b2731345296e2cd796fc5817227a086606027e766306481afd716a3c190283`.
No Development or Test record enters this collection.

For the pilot, select one whole family per human-source collection and
original-draft profile. Rank families by SHA256 of the JSON pair
`["slop-ninja-pangram-family-sampling-v1", source_group]`, then source group.
The source collections cover academic abstracts, news, encyclopedia prose
and narrative writing. Draft profiles include source-matched, plain,
informal, direct, Anti-AI and Fix Slop. Preserve every record from each
selected family. Report original composers and revision routes separately.
The selector and planner validate ancestry, Train assignments and external
evaluation rights before producing exact bulk requests.

## Spend incrementally on useful coverage

After collecting the pilot, inspect score distributions by origin, stage,
collection, draft profile and composer. Select additional unannotated
families from strata that supply the missing interior score ranges, while
retaining some coverage of other strata. Save the prior observations and
the next selection rationale before each submission. Exclude previously
annotated families from subsequent cohorts. Do not repeatedly buy the same
text or alter text merely to manufacture a desired score.

All submitted items, failures and out-of-range scores remain in the corpus.
Any bin-balanced training view must be a separate manifest with its selection
rule and counts. It cannot replace the complete collection. If the available
workflows do not populate a bin, report the gap and reconsider the next
batch before spending the remaining budget. No exact coverage guarantee is
possible before observing the detector.

This adaptive Train collection can identify difficult examples for a later
declared experiment. It is not an unbiased prevalence estimate or a new
benchmark. Keep the existing 57-text Development comparison separate, and
retain the frozen v6 fitting and Test protocol without Pangram supervision
or feedback. Under the project's publication policy, Pangram scores are kept
privately and not published, and they never become origin labels; Pangram
links and ids may be published where they serve verification. Any
teacher-supervised fit needs its own protocol, as in
[the original collection plan](PANGRAM_CORPUS_EXPANSION.md).

Use the existing cumulative budget ledger, durable pre-submission
reservations and single-submission policy. Preserve exact inputs and all
returned versions, hashes and receipts; collect results before the provider's
48-hour retention deadline. Run sampling, builds and other intensive work on
the Studio.

## Pilot results and second cohort

The first cohort returned 120 successful Pangram 4.0 observations from 24
families. All four source collections and six profiles were represented.
Original composers were OLMo (12 families), Qwen (9) and Mistral (3).
Its spend is recorded in the private ledger.

| Origin or stage | Below 60% | 60-70% | 70-80% | 80-90% | 90% to below 100% | 100% |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| Human controls | 24 | 0 | 0 | 0 | 0 | 0 |
| Original model drafts | 0 | 1 | 2 | 0 | 0 | 21 |
| Human-text edits | 1 | 1 | 1 | 1 | 0 | 20 |
| First model revision | 0 | 0 | 0 | 0 | 0 | 24 |
| Second model revision | 0 | 0 | 0 | 0 | 0 | 24 |

All 24 human controls scored zero. The six nonhuman examples in the requested
interior range span 60.6-85.5%; a seventh scores 51.4%. All 48 successive
revisions score 100%. The observed variation comes from original drafts and
human-text edits. These small source/style groups do not establish a general
effect of a model or instruction.

Bindings: pilot input
`eaf7aeb6ccadb19dbb542b313d17a2f02d9715882b098f792c381418e091f8fa`,
plan `0aee07ea557c4d904a734bace23b33a1ac9133188cb679e1f804194482ef24d8`,
annotations `c1f2a085eea1585ae7e87d5d0ff5aa260c2474f5e0c5926d48ebaf0f503c4a05`.
All 120 texts are new relative to the earlier 57-text Development collection.

Before selecting or submitting the second cohort, change its input to the
already frozen 471-family Train seed export, SHA256
`d0cfb18e850615def38c5854697a2e2088428fd4ef1a6dce92147f68cfd1a764`.
Each complete seed family has a human root, original draft and human-text edit.
This also permits the 17 original families whose later revision chain failed;
no source is reassigned and no text changes. Keep every seed record from each
selected family, exclude all pilot families, and use the same sampler/ranking.
This avoids spending on additional revision stages after their uniform pilot
scores, while preserving those pilot observations in the complete collection.

Select up to four new families from each of eight declared collection/profile
groups. Five supplied the pilot's lower scores: narrative/direct,
narrative/Fix Slop, PLOS/direct, PLOS/Fix Slop and PLOS/informal. Three provide
additional exploration: narrative/Anti-AI, encyclopedia/informal and news/plain.
This gives at most 32 new families and 96 texts. Record any exhausted groups,
actual composer balance, exact plan and reservation before submission. Review
the second cohort's distribution before choosing a third; the fixed budget
need not go to uniformly high scores.

The second cohort returned all 96 results successfully, again at version 4.0.
It adds four examples between 70% and 90% and five below 60%. The combined
216-text collection contains 56 human controls and 160 nonhuman texts; the
nonhuman bin counts are 6 below 60%, 2 at 60-70%, 5 at 70-80%, 3 at 80-90%,
none at 90% to below 100%, and 144 at 100%. All human controls score zero.
Its spend is recorded in the private ledger.

Before a third cohort, investigate longer existing texts. The 16 nonhuman
texts below 100% in the first two cohorts are 80-308 words long. Their
reported human fractions range from 12.8% to 67.2%; the corresponding
human-labeled windows contain 24-129 words. This suggests that passage
length and segmentation may limit coverage near 90-99%, but it does not
establish a minimum segment size or guarantee a result for longer writing.

The third cohort will contain every previously unannotated Train seed
family whose original draft has at least 400 words. There are 15 such
families before excluding earlier cohorts. Use word counts already emitted
by the Rust planner for the unchanged v5 Train corpus, whose input hash is
`61c46872d730563bd8531e1a7e2a705afa9d7595f5b74dcee1e2dabbd12331df`.
Freeze the length-based membership and retain each selected seed family's
three original records byte for byte. Validate the resulting ancestry and
rights with the Rust tools before planning its requests. No concatenation,
padding, rewriting or substitution is part of this probe. Report the actual
count, cost and results before deciding on further spending.

The longer-text cohort returned all 42 observations from 14 new families.
Seven of its 28 nonhuman texts score between 66.7% and 100% exclusive,
including three in the previously empty 90-99% band. Its spend is recorded in
the private ledger. This supports collecting more
longer passages, without establishing passage length as the cause.

The [first three cohorts' aggregate](results/pangram-dataset-v2-first-cohorts.json)
contains 258 unique texts from 70 Train families, with 70 human controls,
118 model-only texts and 70 mixed-origin texts. The 188 nonhuman results are:

| Detected content | Texts |
| --- | ---: |
| Below 60% | 6 |
| 60-70% | 3 |
| 70-80% | 6 |
| 80-90% | 5 |
| 90% to below 100% | 3 |
| 100% | 165 |

All human controls score zero. The collection has observations across the
requested range, but remains dominated by 100% results. Every result and
source-family relationship is retained. The spend of these cohorts is recorded
in the private ledger.

A source-disjoint comparison with the frozen v6 detector is planned within the
same budget. Define benchmark
membership independently of detector scores and
retain v6's fixed selection/Test procedure. The next Train length group to
consider is the 39 unannotated seed families with 300-399-word drafts, followed
by additional source/style coverage.

The fourth cohort submitted all 117 seed records from those 39 families. Its
spend is recorded in the private ledger. The provider returned all 117 results successfully, but the
strict collector stopped on one text echo: five source soft hyphens were removed.
Both result pages and the original request are archived. Before admitting this
cohort, validate all responses with a separately recorded, explicit option that
permits only removal of source U+00AD characters and the already allowed
whitespace normalization. Preserve both text hashes and mark such observations
as normalized rather than exact. Other textual changes must still fail.

The user then requested broader generator coverage and model-specific author
labels. [The attribution plan](GENERATOR_ATTRIBUTION.md) directs the remaining
Train allocation toward GPT and Claude as well as the existing local models.
The next 24-source set is frozen independently of its future detector results.
Do not spend that allocation on another expansion limited to the original three
composers. The allocation for a fresh comparison remains available.

The fourth collection now passes the Rust collector with that explicit
normalization policy. It has 13 exact echoes, 103 whitespace-only echoes and
one echo with source soft hyphens removed. No additional request was submitted.
Its annotation SHA256 is
`f907378843135e871e9a596b8936f4f803957a9a1f6a5efb25ab4c4b51a62fcd`.
All 375 Train texts now have validated Pangram 4.0 observations, across 109
source families. The 109 historical human controls all score zero.

| Detected content | Model-only | Mixed | Combined nonhuman |
| --- | ---: | ---: | ---: |
| Below 60% | 2 | 8 | 10 |
| 60-70% | 2 | 2 | 4 |
| 70-80% | 4 | 5 | 9 |
| 80-90% | 5 | 6 | 11 |
| 90% to below 100% | 4 | 5 | 9 |
| 100% | 140 | 83 | 223 |

The [four-cohort report](results/pangram-dataset-v2-four-cohorts.json) retains
the counts, bindings and cost estimates. There are now 33 nonhuman texts in
the requested interior range. The original inputs and their provenance are
available in the [public snapshot](../../datasets/detector-origin-v1/README.md).
Individual provider reports and per-record weighting files remain local.

The prepared weighting view retains all 375 records and full generator ancestry.
It gives each origin equal total weight, then equalizes occupied score bins
within each origin. Its maximum weight is 10.4167 and its weight effective
sample size is 99.9650, computed as squared total weight divided by the sum
of squared weights. That number does not account for within-family correlation.
The sparse bins still need additional examples; weighting them more heavily
does not supply new evidence. No detector has been fitted on these weights,
and the frozen v6 experiment remains unchanged.

On 2026-11-01 the report's limits text was restated under the project's
publication policy, and its source digest now binds the restated weighting
program. The counts, bindings and weights are unchanged.

The [matched GPT/Claude batch](GENERATOR_ATTRIBUTION.md#pangram-annotation-of-hosted-captures)
now adds 96 validated annotations under a separate capture schema. It retains
unknown hosted settings and the capture-time rights flags instead of
manufacturing admitted origin records. Training admission for these outputs
comes only from the [hosted admission manifest](../../docs/a-hosted-output-admission.json). Nineteen new texts
fall between 60% and 100% exclusive, 22 below 60%, and 55 at 100%. All are
preserved with their recorded model-only workflow labels, including cases
Pangram describes as AI-assisted or does not flag.

This batch's spend is recorded in the private ledger. Under the project's
publication policy, Pangram scores are kept privately and not published, and
they never become origin labels; Pangram links and ids may be published where
they serve verification. The new observations do not alter v6 fitting,
selection, calibration or Test decisions.

After completing that hosted batch, the fifth cohort adds 110 Train texts
from 22 previously unannotated source families. It uses the existing closed
revision chains and covers the remaining collection/profile cells, with one
family per available cell. Two PLOS cells had no remaining family. All 110
results validated: 52 exact echoes and 58 whitespace-only echoes. The complete
inputs are published in the [revision snapshot](../../datasets/detector-revisions-v1/README.md).

The [fifth-cohort aggregate](results/pangram-dataset-v2-fifth-cohort.json) shows
limited progress toward filling the interior score range. Of 88 nonhuman
texts, 78 score 100%, eight score between 60% and 100%, and two score below
60%. All 22 human proxies score zero. Of the 66 model-only texts, 63 score
100%; all 22 second revisions are in that bin. The 100% bin allows the
existing tolerance for floating-point sums of AI and assisted fractions.
Retain every observation. These results do not establish that revision improves
detector evasion or that high weights for sparse bins supply more evidence.

The collection now covers 581 distinct Train texts from 155 families, including
the 96 hosted captures under their separate capture schema. Another 57
Development texts remain separate. The spend of the fifth batch and of all eight
jobs is recorded in the private ledger. The fresh v6 comparison keeps its planned
share of the budget, and no further expansion limited to the original three
composers is planned. These annotations and their prepared weighting
view do not change v6 fitting, selection, calibration or Test decisions.
