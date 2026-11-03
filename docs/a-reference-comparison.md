# Restoring the stronger A reference

The next A experiment compares the frozen V6 text detector, the fitted native
CFG-only model and a text-plus-CFG candidate. The grammar and document fallback
remain frozen. All current A outputs must consume one common learned document
encoding; a hybrid experiment does not relax the origin, share, attribution or
Rust serverless inference requirements.

## What the history establishes

Commit `2dd3150694dccfe4002a68ff9ffa7429fff96562` records the
[V6 release results](../baseline/detector/releases/v0.6.0/RESULTS.md). Its
ModernBERT-base classifier has 149,607,171 parameters and used 3,833 Train
records from 471 families. The released model estimates three origin classes;
it has no trained fractional-share or writer/generator attribution heads.

On the historical R2 view, V6 had **0.03264 Brier**, **95.1% accuracy at 0.5**
and 0.1007 log loss. That binary view contains 224 human/model-only rows from
112 families and excludes mixed rows. At its frozen stricter operating point,
it flagged 3/112 human texts and detected 109/112 model texts. The separate
all-human diagnostic flagged 3/117. These are different denominators and
thresholds from the [current shared CFG baseline](../grammar/crates/grammar-a-baseline/SUPERVISED_BASELINE.md).
The current Brier includes mixed examples, and its training cohort is much
smaller. Comparing the historical scores cannot isolate feature choice.

V6 also remained behind Pangram on its paired 162-text comparison: the strict
V6 cutoff detected 98/108 model-only and 29/36 mixed texts with 0/18 human flags;
Pangram's separate content-fraction rule detected 108/108 and 34/36 with 0/18.
Its later leading-space diagnostic exposed large prediction changes. Restore
its strengths while retaining these limitations and formatting checks.

Commit `e6b4b4fad547212648b9feae61c6d9d93b5fec52` records the
[older feature-head comparison](../baseline/detector/FEATURE_NN_RESULTS.md).
These were word/bigram and **spaCy-derived grammatical features**, not native
CFG forest features. A roughly parameter-matched combined head improved
Development log loss from 0.19172 to 0.17137 over word-only; grammar-only was
0.35901. However, combined-512 flagged only 8/96 hosted outputs at its stricter
threshold versus V6's 68/96. Those outputs had Train source roots and no human
controls. This is limited transfer evidence, not general accuracy. The study
used one seed and did not implement fusion with the pretrained text encoder.

## Prepared matched diagnostic

The Rust `prepare_reference_replay` example binds exact text requests and an
audit roster to the current dataset manifests. It checks current source
families, exact text hashes and whitespace-normalized hashes against every
published V6 Train, Development, Calibration and Test record. Training overlap
takes precedence over selection overlap, which takes precedence over prior
Test evaluation. References are excluded; only `id` and `text` reach inference.

Measured October 26, 2026 on build-host:

| Current population | Queries / families | V6 selection or Calibration exposure | Previously evaluated V6 Test | No supplied-roster match |
| --- | ---: | ---: | ---: | ---: |
| Development | 106 / 16 | 50 | 0 | 56 |
| Calibration | 107 / 16 | 50 | 0 | 57 |
| Already-opened Test | 102 / 15 | 0 | 45 | 57 |

No query in these populations matched V6 Train by these checks. Absence of a
match does not exclude pretraining exposure, alternate editions or unrecorded
overlap. In particular, historical literary sources are not authenticated
contemporary human-only workflows. The current Test is already consumed; this
replay is retrospective and cannot select the next model or establish a new
clean holdout. Report its exposure groups separately.

All 315 requests now have recovered V6 predictions, with no unsupported inputs
or execution failures. The original release remains unchanged. A separately
identified [Linux CPU profile](platform-support.md#v6-linux-cpu-runtime) passes
the original bundled reference checks without changing their tolerance.
Empty and overlength synthetic requests remain unsupported; no truncation is
performed. Exact requests, audit rosters, manifests, runtime receipts and
source hashes are retained under ignored
`data/grammar-private/a-parity-stage2-20261026/`.

```sh
prepare_reference_replay RECORDS.jsonl MANIFEST.json development V6_DATA_DIR NEW_OUT
prepare_reference_replay TEST_RECORDS.jsonl TEST_MANIFEST.json test V6_DATA_DIR NEW_OUT --opened-test
```

Outputs must remain in Git-ignored `data/grammar-private/`. V6's exact original
artifact and runtime must be hash-verified and its reference predictions replayed
before scoring. A new Linux/CUDA runtime gets a distinct binding. Preserve
unsupported/failed requests, particularly the old 2,048-token limit; never
silently truncate. Its old probability calibration and strict/loose thresholds
remain frozen in the reference comparison. Any new Calibration-only mapping
is a separately identified result. Do not infer share or attribution from V6.

## Matched recovered-model results

The table uses equal source-family weighting within each metric's eligible
rows. CFG is the previously selected leaf-grammar joint head; its range covers
all three already-frozen seeds, not a newly chosen best seed. Pangram means
its saved content-fraction observations after the existing Calibration-only
mapping to involvement. Lower Brier is better. All methods see identical exact
documents; V6's old calibration remains frozen.

| Population | Documents / families | Frozen V6 Brier | CFG Brier | Mapped Pangram Brier |
| --- | ---: | ---: | ---: | ---: |
| Development, all origins | 106 / 16 | 0.10272 | 0.12308–0.13986 | 0.07705 |
| Consumed Test, all origins | 102 / 15 | 0.13959 | 0.12582–0.13412 | 0.09574 |
| Consumed Test, human/model only | 57 / 15 | 0.02283 | 0.16589 (seed 17) | 0.01073 |
| Consumed Test, mixed only | 45 / 15 | 0.23824 | 0.07361 (seed 17) | 0.23345 |

V6 recovers strong pure-origin discrimination, but its confident misses on
mixed examples substantially worsen involvement Brier. CFG's better mixed-only
Brier accompanies frequent human false positives: at 0.5, family-weighted Test
human FPR is 66.7% for CFG seed 17, 6.7% for V6, and 0% for mapped Pangram.
V6's frozen stricter cutoff yields zero observed human flags here with 80.7%
family-weighted AI recall. There are only 21 human documents from 15 families;
zero observed flags is not a rare-false-positive guarantee.

Exposure makes a large difference. On the 57 Test documents from six families
with no supplied V6-roster match, V6 Brier is 0.27222, CFG seed 17 is 0.15237,
and mapped Pangram is 0.14842. On the 45 previously evaluated V6-Test documents
from nine families, these are 0.05117, 0.10813 and 0.06063. The unmatched group
contains proportionally more mixed examples, so this difference does not
isolate exposure from production history or genre. Development likewise
contains 50 V6 selection/Calibration-exposed documents. These populations
cannot establish general Pangram parity.

On all 102 Test documents, the paired V6-minus-Pangram Brier gap is +0.04384
(95% source-family bootstrap interval −0.00952 to +0.11071). CFG seed 17's gap
is +0.03008 (−0.00630 to +0.06567). Intervals spanning zero do not establish
equivalence. The [machine-readable aggregate report](a-reference-comparison-results.json)
retains all seeds, calibration bins, three-way origin metrics, separate
production/exposure strata, paired uncertainty and input/output hashes.

```sh
python baseline/detector/ml/export_v6_cpu_runtime.py \
  --parent RECOVERED_ENCODER --output NEW_CPU_VARIANT \
  --cpu-kernel-profile portable-compatible
# Install the variant's pinned dependencies in an isolated Python 3.12 runtime.
python NEW_CPU_VARIANT/runner/infer_compatible.py \
  --artifact NEW_CPU_VARIANT < PREPARED/requests.jsonl > predictions.jsonl
score_reference_replay --prepared PREPARED --v6-predictions predictions.jsonl \
  --v6-artifact NEW_CPU_VARIANT --cfg-fit FROZEN_CFG_FIT \
  --pangram-comparison SAVED_COMPARISON.json --pangram-results SAVED_RESULTS \
  --out NEW_REPORT.json
```

The scorer checks exact-text, source-family, artifact, weight and saved-response
bindings. Its four synthetic tests include invalid probability rejection,
family weighting, mixed involvement semantics and unavailable empty metrics.
No provider calls are made by preparation, scoring or automated tests.

## Pangram retention and subsequent fitting

Saved Pangram observations already cover every requested row, so this diagnostic
requires no new credits. Reuse only observations bound to identical exact text
and the declared request/version. Preserve every repeated observation rather
than averaging away provider variance. The existing score mapping was fitted on
Calibration, not Test, and is not Pangram's native any-AI probability.

Every new paid attempt must produce a durable offline fixture: exact source and
submitted text, text hash, production/span provenance, request settings,
provider and returned model versions, raw response or failure, task identity and
timing. Preserve submission intents so interrupted requests cannot be blindly
resubmitted. Keep raw observations independent of later score mappings. These
are detector observations, never authorship or realized-share ground truth.
Private and restricted material stays ignored; public regression fixtures must
be synthetic or independently authorized. Automated tests must not call the API.

The fitting comparison should keep data, head capacity and training budgets
matched between text-only and text-plus-native-CFG, with repeated seeds and a
CFG-only control. All heads use the shared encoding. Fit and select only on
the new experiment's Train, Development and Calibration populations; reserve
new source families and withheld identities for final Test. Retain native
fallback and missing-feature signals, independent reference works and measured
fractional rewrite spans. Score AI involvement first and report share and both
attribution roles separately. A cached text embedding is not free first-time
inference: the complete text encoder's operators, model size, CPU latency and
peak memory must also satisfy the Rust/Workers-or-Container constraint.

The [first fitted text-plus-CFG comparison](a-v6-cfg-fusion.md) is complete:
21 bounded fits and numeric Rust replays on the existing non-Test cohort.
Text features improve the exposed Development result substantially; the
incremental CFG involvement gain is small and uncertain. The experiment retains
all required shared-A heads and does not establish Pangram parity.
