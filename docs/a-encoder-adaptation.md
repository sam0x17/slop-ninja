# Bounded encoder adaptation

The current experiment compares a frozen V6 encoder against adaptation of its
final block, with identical shared residual A heads, data, seeds and budgets.
Adaptation modestly improves the matched control's balanced Test Brier
(0.072547 versus 0.077550), with no clear gain under the observed descendant
mixture. It remains behind CAL-mapped Pangram on Development and the complete
636-query Test (Pangram balanced Brier 0.048025). General
attribution and Cloudflare serving remain unqualified.

The previous final Test is consumed regression evidence. Its results remain in
[a-final-test.md](a-final-test.md); they cannot select this experiment's models.
All three matched seeds have completed real-data fitting. The first table gives
uncalibrated Development results; the frozen Test comparison follows below.

| Seed | Adapted final block | Matched frozen encoder |
| --- | ---: | ---: |
| 17 | 0.075702 | 0.081275 |
| 29 | 0.076319 | 0.081114 |
| 41 | 0.076485 | 0.078975 |
| Mean | 0.076169 | 0.080454 |

The predeclared mean-score rule selected final-block adaptation. Its paired
adapted-minus-frozen Brier difference is −0.004286, with 95% source-family
interval [−0.010348, +0.000750]. The interval includes no benefit. There are
439 Development documents from twenty works, represented by sixty family/origin
cells in this balanced calculation; the sealed selector's interval field named
`documents` counts those cells. Original V6 initialization scores 0.174393.
All six CAL fits, numeric exports and calibrated Development evaluations
completed. Calibration is fitted diagnostic evidence. On Development:

| Seed | Adapted Brier | Frozen Brier | Adapted share MAE, points | Frozen share MAE, points |
| --- | ---: | ---: | ---: | ---: |
| 17 | 0.077869 | 0.082387 | 12.20 | 12.71 |
| 29 | 0.077617 | 0.082342 | 11.54 | 12.28 |
| 41 | 0.078783 | 0.081074 | 11.92 | 12.38 |

Human FPR at the diagnostic 0.5 involvement cutoff is 7.5% for each adapted
seed and 5%, 7.5%, 10% for the frozen seeds. The adapted human-attribution head's
conditional top-1 ranges from 21.6% to 28.9%, and generator top-1 from 72.9% to
73.7%. It falls back on 95.1–100% of known human roles and 88.2–94.2% of known
generator roles. That frequent rejection prevents calling the attribution
mechanism useful despite its fitted thresholds. Independent-reference human
matching reaches only 19.2–20.9% top-1. There is no separate generator reference
gallery; missing retrieval support stays explicit. All documented contributors
are evaluated regardless of predicted involvement. Per-genre results, share
bins, open-set rejection and family intervals are retained in the private
evaluation reports. Separate frozen Test results follow below.

Fixed comparators and Pangram now cover the same 439 Development documents.
The following Brier scores average all three fixed seeds where applicable.
Balanced weighting gives each source work equal mass and each origin class
within that work equal mass. Family weighting gives each work equal mass while
retaining its observed class mix; this cohort contains many mixed descendants.

| Candidate | Family/origin-balanced Brier | Family Brier |
| --- | ---: | ---: |
| Pangram, CAL-mapped involvement | 0.043816 | 0.084629 |
| Selected adapted A | 0.078090 | 0.114341 |
| Matched frozen-encoder A | 0.081934 | 0.118442 |
| Previous selected A | 0.104205 | 0.085387 |
| Original frozen V6 | 0.174393 | 0.380030 |

The selected A's balanced gap above mapped Pangram is +0.034274, with paired
95% source-family interval [+0.024077, +0.045695]. The family-weighted gap is
+0.029712 [+0.005760, +0.056508]. These Development results do not establish
parity. Pangram's native fraction is neither involvement confidence nor measured
AI share: the table uses an isotonic involvement mapping fitted only on the
637 Calibration documents. All 1,076 paid responses and exact request bindings
are retained as permanent fixtures; none of the inspected archived observations
matched this cohort's exact texts. There are no excluded Development responses.
Pangram and original V6 had zero false positives among the forty human-proxy
documents at the diagnostic 0.5 cutoff; this small sample establishes no rare
false-positive guarantee.

Compared with the previous A, adaptation improves balanced Brier by 0.026115
(paired interval [−0.057684, +0.001593] for adapted minus previous), but worsens
family-weighted Brier by 0.028955 [+0.010726, +0.049837]. It is not a uniform
improvement. The previous A's human FPR is 27.5–30%, versus 7.5% here. Original
V6 retains its unchanged weights and temperature 1.8776105792643432; its mixed
origin evaluation here is separate from the historical binary-only release
metrics. V6 supplies no share or attribution predictions. The previous A's
smaller profile roster and calibration also remain unchanged. No comparator
result changed the already sealed model selection.

The frozen design uses seeds 17, 29 and 41, twelve epochs, logical batches of 64,
AdamW at 0.0003 for the shared heads and 0.00001 for the adapted final block.
Layers 0–20, final normalization and the existing prediction head remain frozen.
Origin, bounded share and both conditional attribution heads use one shared
768-dimensional representation. CFG coordinates are zero in both conditions;
this is not another CFG representation comparison. Every source work has equal
loss mass, with balanced origin cells within it. The original twelve works no
longer receive a special half of the expanded training loss.

The new metadata reservation contains 98 works from 14 writers, across fiction,
nature writing, travel, essays and history/biography. Twenty query works and two
withheld-writer reference works are reserved for Test before any prose acquisition.
The 76 acquired non-Test works yielded 128 fixed-location query excerpts and 24
independent reference excerpts, without extraction failures or overlap exclusions.
Train has 48 new excerpts; Development and Calibration each have 40. Each latter
population has twenty works, eight known writers and two different withheld
writers. Existing retained Train contributes 3,629 queries from 333 families.
References never fit weights or normalization.

Rights are backed by retained Project Gutenberg RDF identifying US public-domain
works, exact HTTP bodies and notices. These are historical human proxies, not
authenticated contemporary human-only production logs. Composition dates and
foundation-model exposure remain unknown. Work aliases, cumulative exact and
whitespace-normalized excerpt hashes, and shared fifty-word sequences among new
excerpts are checked; this is not a universal full-work or pretraining audit.
Conservative unknown grammar exposure remains explicit.

Local Qwen and Phi generators supply known-model examples; Gemma is restricted
to Calibration. A different retained generator is reserved for Test and has
prior regression exposure. Controlled replacements request 10%, 40%, 80% and
100% of source spans at different sentence positions. Supervision uses the
measured fraction of final words in replacement spans. A complete rewrite still
has mixed production history. Every request, response, failure and exact splice
is retained. Generation finished with 1,476 accepted of 1,480 requested outputs;
four bounded failures remain in the denominator. The assembled new population
contains 528 Train queries, 439 Development queries, 637 Calibration queries and
24 independent references. Combining only the retained Train rows gives 4,157
training queries from 357 source works, with fourteen supported writer identities
and two supported generators. Development and Calibration each contain twenty
source works. All 5,257 retained/new rows have supported prefixes without
truncation; no row was removed for encoder length.

The rights-checked [published encoder cohort](../datasets/a-encoder-20261026-v1/README.md)
retains 2,268 records from 98 works: the new non-Test population above, 636
consumed Test queries and four Test references. Six lossless payloads occupy
2,769,109 compressed bytes, preserving 38,943,235 decoded bytes including the
evidence index. Exact measured spans, production histories, source rights,
generator versions and fixed splits remain; private profiles, weights and live
provider logs stay outside Git. This is exposed research evidence, not a fresh
hidden benchmark. The [retained Train publication](../datasets/a-retained-train-20261026-v1/README.md)
preserves another 2,365 queries from 225 works in 4,169,964 compressed payload
bytes, including its evidence and reconstruction indexes. Its
[training reconstruction manifest](../datasets/a-retained-train-20261026-v1/training-reconstruction.json)
locates all 4,157 fitted Train queries from 357 works across four published
snapshots, preserving original fit order, exact and normalized text hashes,
source families, origin, involvement, realized share and missingness, both role
identities/presence, known masks and attribution target indices. The independent
retained-input audit matches all 3,629 reused queries with zero missing rows.
The original fit contract and Train-file checksums remain bound; no fitting,
model selection, calibration or predictions changed. Restore the compressed
payloads before reading the membership JSONL or checking original checksums.

On build-host's RTX 4090, the CUDA qualification matched bundled V6 reference
probabilities within 7.8e-7. A discarded synthetic training update changed only
the six final-block parameter tensors. Full and cached-prefix execution agreed
exactly in those cases. All 3,629 retained Train documents then produced supported
FP32 prefixes, each checked against full execution at tolerance 1e-6: 70.996
seconds, 803,921,920 bytes peak CUDA allocation, approximately 3.7 GiB on disk.
These are preprocessing measurements, not training or production CPU latency.

The trainer's synthetic test verifies that streamed document gradients match the
existing full-batch masked joint objective. It saves atomic optimizer/RNG recovery,
epoch-one checkpoints, predictions and learning curves. The first adapted run's
CUDA allocation peaked at 999,627,264 bytes; its frozen control peaked at
696,067,584 bytes. All runs use one CPU, a 10 GiB RAM cap, zero swap and a 50%
per-process CUDA allocation ceiling. Earlier layers stay fixed, allowing exact
cached-prefix reuse; these timings do not represent first-time inference.

The early epoch-one derivative passed three synthetic full-encoder Rust cases
against its CUDA training runtime: maximum output error 3.64e-6 on the 0–1 scale,
raw encoding error 9.85e-5 and shared encoding error 1.00e-4, within the unchanged
1e-5/1e-4/1e-3 gates. Two CPU-PyTorch oracle runs exceeded the raw-encoding gate
(1.67e-4 and 1.58e-4), although their output errors passed. Those failures are
retained; CPU PyTorch interchangeability is not qualified. No case or tolerance
was changed. This small CUDA-reference check does not replace qualification of
the final selected weights or the full input boundary roster.

The early Rust process used 1,173,652 KiB peak RSS and took 2.55 seconds for all
three synthetic cases, including verification/loading. Encoder forwards totaled
1.718 seconds; the common A heads totaled 0.000429 seconds. This is a small mixed
length batch, not a latency percentile or a 2,048-token benchmark. The retained
parent weighs 598,442,860 bytes, the separate final-block overlay 20,060,672 bytes,
and this head export 5,023,733 bytes. Production inference needs no Python or GPU.
Workers/Wasm and Cloudflare Container qualification remain outstanding.

Final calibrated CUDA-reference replay passes output and shared-encoding gates
for all six runs. All frozen runs pass the raw-encoding gate too. The adapted
runs slightly exceed its unchanged 1e-4 gate (maximum errors 1.0681e-4, 1.0514e-4
and 1.0896e-4 for seeds 17, 29 and 41). Their maximum output errors remain below
6.8e-6. These are retained qualification failures requiring investigation;
the calibration runner's successful exit does not mean every comparison passed.

A separate Linux CPU profile passes all six runs on 44 fixed synthetic
CUDA-reference cases each: 39 supported inputs and five explicit unsupported
inputs. These include 2,048-token execution, rejection at 2,049 tokens, Unicode,
empty input, and attention-window boundaries. Matching MKL 2024.2 and PyTorch
2.9.1's DEFAULT LayerNorm and softmax reduction order gives maximum errors of
3.6240e-5 for raw h, 4.0514e-5 for shared z, and 7.6242e-6 for outputs on the
0–1 scale. The unchanged gates are 1e-4, 1e-3, and 1e-5 respectively. Earlier
FP64, MKL-only and default-Candle failures remain retained; no weights or
tolerances were changed to obtain these results.

The profile requires native Intel libraries and is not a Workers/Wasm backend.
On build-host with one CPU thread/quota, adapted seed 17 took 18.96–19.07 seconds
for each 2,048-token encoder forward; shared heads took about 0.13 milliseconds
separately. Across six runs, peak process RSS was 1,366,160–1,374,676 KiB. These
are full-model synthetic diagnostics on a shared host, not isolated deployment
benchmarks or a throughput guarantee. Parent weights occupy 598,442,860 bytes,
the layer overlay 20,060,672 bytes, and seed-17 heads JSON 4,982,039 bytes.
The current footprint exceeds a Workers isolate's 128 MiB memory budget;
bounded Container packaging/performance remains unqualified. The completed
`boundary-qualification-v1` receipts retain per-case latency, load/hash time,
resource caps, predictions, hashes and all acceptance checks.

The original recovered release stays immutable. Fixed V6/prior-A and Pangram
Development comparisons are complete. New Test evaluation of both matched
conditions, fixed V6/prior-A and Pangram is complete. Cloudflare serving remains
unqualified. A Test seal binds 120 artifacts: all six
weight/calibration exports, the Development selection, fixed comparators,
CAL-only Pangram mapping, source reservation, generator files and evaluation
plan. Only afterward were the twenty query works and two independent reference
works acquired, yielding forty query excerpts and four references without
extraction failures. Source admission checked 13,619 retained text occurrences
and excluded none of those excerpts. Generated-text and scratch-prompt overlap
are checked separately after generation. No Test metric may reselect the model
or change calibration.

All 600 requested Test generation tasks completed successfully. Assembly kept
636 of 640 query records across twenty works, plus four independent references
from two other works; four generated duplicate/no-change records remain explicit
exclusions with their evidence. The generated-text audit found no conflicts
against 13,619 protected text occurrences and 25,602 prompt occurrences. All
retained inputs fit the unchanged 2,048-token limit; no truncation or execution
failures occurred in prefix export or the six runs.

On this Test, mean involvement Brier across all three seeds is **0.07254665**
for adaptation versus **0.07755037** for matched frozen encoding under equal
family/origin weighting. The paired difference is **−0.00500372**, with a
source-family bootstrap interval **[−0.00935266, −0.00148005]**. Under the
observed example mixture with equal family weighting, the means are
**0.10610122** and **0.10592866**: difference **+0.00017256**, interval
**[−0.00403785, +0.00514547]**. Thus the gain is concentrated in the balanced
criterion and is not a uniform improvement across weighting choices. These
are averages of per-document losses across seeds, not an ensemble or best-seed
result; the uncertainty resamples twenty works, not 636 independent documents.

Adapted share MAE is 11.55–12.43 percentage points versus 12.92–13.16 for the
matched frozen condition. Adapted bias is −5.33 to −8.06 points. Human false
positives at involvement probability 0.5 are 5–7.5% across adapted seeds versus
7.5–15% for frozen, measured on only forty historical human-proxy excerpts.
These results do not establish a rare-false-positive guarantee, reliable
production-history inference, or Pangram parity. All role and reference metrics
remain in the per-seed reports. `final-test-v1/evaluation-v1` retains the first
report; `evaluation-v2` corrects the displayed balanced count to distinguish
636 documents from sixty family/origin cells, using identical predictions,
parameters, loss weighting and bootstrap computation.

The fixed comparators support the same 636 queries without execution failures.
V6 keeps its original encoder, classifier and temperature. Previous A keeps its
original weights, normalization, profile roster and Calibration parameters.

| Candidate | Test family/origin-balanced Brier | Test family Brier | Human FPR at 0.5 |
| --- | ---: | ---: | ---: |
| Selected adapted A | 0.072547 | 0.106101 | 5–7.5% |
| Matched frozen-encoder A | 0.077550 | 0.105929 | 7.5–15% |
| Previous selected A | 0.090704 | 0.070540 | 32.5% |
| Original frozen V6 | 0.168267 | 0.375944 | 0% |
| Pangram 4.1, unchanged CAL mapping | 0.048025 | 0.073476 | 2.5% (1/40) |

Brier entries average losses over the three fixed seeds for A candidates;
V6 and Pangram each have one fixed comparator. Adapted-minus-V6 balanced Brier is −0.095721, with paired
source-family interval [−0.117050, −0.075120]. Adapted-minus-previous-A is
−0.018157 [−0.041832, +0.002941] for balanced Brier, but +0.035561
[+0.010421, +0.060380] for family Brier. Previous A also has lower overall share
MAE, 10.03–11.17 points. Adaptation reduces its human flags while worsening
performance on the observed descendant mixture; this is not a uniform gain.
V6's absent share and attribution outputs are explicitly masked.

Share diagnostics separate the forty human proxies, 120 model-only documents
and 476 mixed-history documents. Adapted MAE is 4.03–4.87, 0.61–1.01 and
14.88–16.05 points respectively. Mixed-history bias is −10.87 to −7.38 points,
versus −2.24 to +0.72 for previous A; mixed examples remain the main share
weakness. Reports retain MSE, realized-share bins and family uncertainty as well.
These labels come from production history and final replacement-span word
counts, independently of any detector percentage.

Pangram covers all 636 Test queries: forty human proxies, 120 model-only and
476 mixed-history documents. After an explicit resume receipt, true bulk
collection completed 607 requests and reused the earlier 29 results. The one
old failed POST had a complete, hash-bound provider rejection response and no
task receipt; explicit reconciliation preserved that evidence before reissuing
it. No genuinely uncertain POST was repeated. All 607 bulk
items succeeded and returned version 4.1, matching the sealed Calibration
mapping. Forty-four response echoes were byte-exact and 563 differed only in
whitespace; submitted bytes and every raw response remain retained. All 1,712
CAL/Development/Test results are permanent fixtures. Billing estimates use the
documented bulk unit definition.

Using the unchanged mapping, Pangram's Test balanced Brier is **0.04802542**,
versus **0.07254665** for adapted A. The paired adapted-minus-Pangram gap is
**+0.02452124**, with source-family interval **[+0.00406349, +0.04100618]**.
With equal family weighting under the observed origin mixture, Pangram scores
**0.07347562** versus **0.10610122**: gap **+0.03262560**,
interval **[+0.01600007, +0.04877316]**. Pangram flags one of forty human proxies
at 0.5, compared with two or three for adapted A. That small human population
cannot establish rare-error guarantees. Calibration bins and paired results
for all four fixed comparators are retained in `pangram-evaluation-v2`; the
earlier partial report remains unchanged. These are our CAL-mapped involvement
probabilities, not Pangram-native confidence or share ground truth. No model,
threshold or mapping was reselected.

The bounded experiment supports a modest balanced improvement from adapting
V6's final block, not parity or useful general attribution. It supplies no new
CFG benefit evidence because CFG coordinates remain zero. The complete paired
comparison still favors Pangram under both declared Brier weightings. Further
acquisition emphasizes independent works, varied generators and source-compatible
free-form rewrite instructions; collection is separate from fitting. Any next
fitting experiment needs fresh selection and
Test populations; these inspected Test works are now consumed evidence.

Task evidence and recovery commands are under
`data/grammar-private/a-encoder-stage3-20261026/`, including `HANDOFF.md`,
`protocol-v1.json`, `FROZEN_INPUTS.sha256`, source/HTTP receipts and job logs.
Campaigns remain paused; their independent recovery instructions remain at
`data/grammar-private/a-baseline-20261026/pause/RESUME.md`.

`audit_last_block_evidence STAGE NEW_REPORT.json [PANGRAM_REPORT_DIR]` checks the retained experiment
offline. It reconstructs the original 120-file Test seal, rehashes bound artifacts,
checks source-group separation across query/reference populations, validates all
six runs' calibrated prediction contracts, and checks the retained Linux numeric
qualification against the same fitted weights. It preserves the distinction
between 640 requested queries, four upstream exclusions and 636 evaluated
queries, and between report completion and Pangram population coverage.
Its output stays under the private task root. It does not rerun inference,
fit anything, make provider calls, or assert overall-goal completion; retained
qualification results are not a fresh replay. Generator-reference retrieval,
contemporary human-only histories and remote backup remain separate limitations.
The completed `evidence-audit-v3` run passed five focused tests and rechecked
841 bound artifacts, including all 636 provider receipts and all 3,816 calibrated
Test predictions against the complete provider report. The original
frozen-input and fitting/calibration/qualification checksum lists also pass.
The requirement matrix and exact file bindings remain alongside its private
report; earlier audits remain retained, and no weights, predictions, source
populations or selection changed.

Build that Rust example with one Cargo job in a bounded task, then run:

```sh
cargo build --release --locked --offline --manifest-path grammar/Cargo.toml \
  -j 1 -p grammar-a-baseline --example audit_last_block_evidence
grammar/target/release/examples/audit_last_block_evidence \
  data/grammar-private/a-encoder-stage3-20261026 \
  data/grammar-private/a-encoder-stage3-20261026/new-evidence-audit.json
```

[Persistent CFG evidence](a-data-storage.md) is now cached independently of
fitting: exact-text vectors are bound to the frozen parser/feature profile, and
compressed source/forest archives support later reuse. This backfill does not
alter the current comparison's constant-zero CFG coordinates. The already
evaluated Test is queued separately for preservation, without opening a new holdout.

The reusable model entry points are
`grammar/crates/grammar-eval/python/last_block_a.py` (qualification),
`last_block_prefixes.py` (resumable preprocessing), and `train_last_block_a.py`
(matched fitting). Rust examples `a_freeze_encoder_sources`,
`prepare_encoder_train_requests`, `admit_encoder_sources`, and
`assemble_encoder_dataset` own admission and corpus handling. The fitting entry
point consumes the frozen `slop_ninja_last_block_a_dataset_v1` bundle produced
by `prepare_last_block_fit`. That Rust join binds each exact text to its tensor
bytes, derives the named profiles only from Train and fits normalization only
on Train queries. `select_last_block_condition` seals the three-seed Development
choice before `predict_last_block_a.py` can expose CAL model outputs;
`calibrate_last_block` fits the CAL-only temperature and named thresholds.
`export_last_block_a.py` exports numeric heads, the six changed encoder tensors
and synthetic reference predictions. The native Rust entry points are
`inference/a-v6-cpu` binaries `last_block` and `compare_last_block`.

The completed fitting, calibration and evaluation runners are recorded under
`fit-run-v1/`, `calibration-run-v1/` and `evaluation-job-v1/` in the task root. These are fixed
experiments, not an architecture search. A first launch failed on a working
directory before fitting; the preserved corrected launch revalidated every
frozen input. A calibration build required an explicit numeric type annotation;
the focused tests and corrected build passed before calibration was queued.

Fixed comparisons are reproducible through `prepare_last_block_comparators`,
`predict_frozen_v6_head.py` and `evaluate_fixed_last_block_comparators`.
`compare_last_block_pangram` validates permanent exact-text observations and
fits the CAL-only mapping. Captured commands, binaries, source hashes and exits
are retained in `comparators-job-v1/`, `comparator-evaluation-job-v1/`,
`pangram-job-v1/` and `pangram-evaluation-job-v1/` under the task root. These
comparison programs perform no live calls; collection is a separate explicit
operation. The comparison/evaluation suites passed 29 and 47 focused/inherited
tests respectively. Synthetic layer tracing retains failed numerical experiments
without changing training weights, reference cases or acceptance tolerances.

The separate final-Test route uses `freeze_last_block_test` to bind the completed
selection before acquisition, `admit_last_block_test` for source/rights/overlap
checks, and `a_balanced_generate --recipe encoder_stage3_test_v1 SEAL.json` for
the frozen 10/40/80/100-percent source-span recipe plus independent scratch
generation. Share labels remain realized final-word fractions. Assembly verifies
every terminal task, response, exact splice and source-family binding; the
sealed overlap policy excludes entire conflicting families with receipts and
no replacement sources. Missing and unsupported cases remain in the population
accounting. The existing non-Test entry points continue to refuse Test inputs.

After all three generators finish, the Rust commands are:

```text
assemble_last_block_test STAGE_ROOT NEW_DATASET
prepare_last_block_test STAGE_ROOT DATASET PREFIXES NEW_NUMERIC_BUNDLE
evaluate_last_block_test STAGE_ROOT NEW_EVALUATION
```

Between assembly and numeric preparation, the unchanged `last_block_prefixes.py`
captures exact-text model prefixes. Prediction uses `predict_last_block_a.py`
with `--test-seal`, `--fit-data` pointing to the original fitting bundle, and
`--part test` or `--part reference`. The Test contract copies original profile
enrollment and normalization byte-for-byte; it cannot train or recalibrate.
Evaluation applies sealed CAL parameters to all six fixed runs, supplements
independent known references with the separately acquired Test references,
and computes paired source-family uncertainty after averaging losses over all
three seeds. It does not select the best Test seed or report an ensemble score.
Captured bounded runners under `final-test-v1/` advance generation, assembly,
prefix export, prediction and evaluation through durable completion files.

The explicit fixed-comparator Test commands are:

```text
prepare_last_block_comparators --test STAGE_ROOT NEW_PREPARED
predict_frozen_v6_head.py --prepared PREPARED --out NEW_V6 --test-seal SEAL.json
evaluate_fixed_last_block_comparators --test STAGE_ROOT PREPARED V6 NEW_EVALUATION
compare_last_block_test_pangram STAGE_ROOT FIXTURES_TEST_DIRECTORY NEW_REPORT
```

The last command is offline and loads the sealed CAL mapping. Its
`pangram_population_complete` flag is separate from report-generation success.
The 52-test suite covers mapping round trips, tampering, changed provider/input
bindings and uncertain submissions. Completed commands and hashes are retained
in `final-test-v1/comparators-job-v1/` and `pangram-report-job-v1/`; read-only task
recovery is in `pangram-poll-recovery-v1/`. Collector `poll-only test` mode forbids
POST/catalog requests and preserves the latched stop record and all uncertain intents.

The true bulk collector shares the same exact-input, public-source and split
checks. An explicit resume receipt binds the original stop record's hash;
completed results are reused and ambiguous submissions remain excluded.

```text
a_supervised_pangram bulk prepare INPUT MANIFEST SPLIT FIXTURES NEW_JOB RESTORATION.json
a_supervised_pangram bulk submit JOB KEYFILE
a_supervised_pangram bulk watch JOB KEYFILE
```

Each POST has an atomic durable intent, a stable recorded idempotency key, and
automatic retries disabled. `collect JOB KEYFILE` polls once; `watch` has a
twenty-minute budget and can resume the same accepted bulk ID. Raw requests,
pages, task/index bindings, provider versions and canonical per-document results
are permanent fixtures. Provider bulk retention is 48 hours after completion,
so the bounded runner collects promptly. `pangram-bulk-job-v1` retains the
current captured binary, twelve focused tests, official SDK revision, all
receipts and the successful complete comparison. This workflow never changes
the original Test seal or CAL mapping.
