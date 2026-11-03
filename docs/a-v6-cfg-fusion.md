# Frozen V6 features with a shared A head

Measured on build-host, October 26, 2026. Reusing V6's learned text representation
substantially improves this exposed Development experiment over aggregate CFG
features. Adding CFG gives a small, uncertain incremental involvement gain and
better known-generator classification. **Pangram parity, robust attribution and
Cloudflare serving remain unproven.** The next useful experiment needs new
source families and better balanced production histories, rather than a larger
architecture search on these documents.

## Population and frozen experiment

All 450 non-Test records were retained: 229 Train queries, 106 Development,
107 Calibration and eight independent reference excerpts. Train has 169 share
targets, including only 16 fractional targets, 32 named-human targets and 159
named-generator targets. Missing targets remain masked. No new Test was opened;
the earlier 102-document Test remains consumed retrospective evidence in the
[V6 comparison](a-reference-comparison.md). Exact public source text, history,
spans, masks and partitions are now in the
[552-record dataset snapshot](../datasets/a-supervised-20261026-v1/README.md).

V6's frozen mean-pooled classifier input supplies 768 text coordinates. Its
weights, tokenizer, calibration and reference checks remain unchanged; the
[qualified Linux CPU profile](platform-support.md#v6-linux-cpu-runtime) is
separately identified. A read-only classifier hook preserves logits and
probabilities exactly on the bundled references. All 213 Development/Calibration
predictions also matched the earlier comparator replay exactly. No text was
truncated or unsupported. This representation is explicitly raw-text-derived,
not a CFG feature.

Rust fits text-coordinate means and population standard deviations on Train
queries only, equally weighting source families, with a 0.01 scale floor.
Reference, Development and Calibration values do not fit normalization. The
2,062 frozen native grammatical-leaf/count/availability coordinates retain their
original vocabulary, meanings, masks and normalization. Grammar and document
fallback are unchanged. Dictionary word identities are not supplied by this CFG
ablation; incomplete syntax stays explicit in the existing coordinates.

Each candidate has 2,830 inputs, a learned `64 → 32` tanh encoder, and origin,
mixed-share, human and generator heads consuming that same 32-dimensional
encoding. Text-only zeroes the CFG block; CFG-only zeroes the text block.
Architecture and total parameter count match, while active input/parameter
counts differ. All required [A output invariants](../grammar/crates/grammar-a-baseline/SHARED_CONTRACT.md)
remain enforced, including constrained share and both attribution roles without
an involvement cutoff. The frozen text extractor plus this shared head is one
candidate pipeline; V6's separate old classifier is only a comparator.

The protocol was recorded before fitting: seeds 17/29/41, 250 full-batch AdamW
epochs, learning rate 0.001 and weight decay 0.01. Joint, origin-only and
attribution-only controls were run for text and combined features; CFG-only has
three joint runs. All **21 fits, evaluations and Rust numeric replays completed**.
The existing trainer selects origin-bearing checkpoints by Development
involvement Brier, attribution-only checkpoints by conditional identity loss,
and fits temperatures/named-match cutoffs only on Calibration. Controls retain
explicitly untrained outputs; those outputs are not qualified A predictions.

## Results

These are family-weighted Development metrics across every declared seed.
The human FPR operating point is 0.5, not a promised error guarantee. The Train
family prior has Brier 0.16505. Mapped Pangram uses the same saved exact-text
responses and existing Calibration-only mapping; no new paid calls were made.

| Candidate | Involvement Brier | Human FPR | Three-way origin accuracy | Share MAE, percentage points |
| --- | ---: | ---: | ---: | ---: |
| CFG-only joint | 0.12573–0.13122 | 46.9–50.0% | 51.3–62.0% | 30.94–33.05 |
| Text-only joint | 0.05177–0.05834 | 31.3% | 71.8–73.9% | 12.73–15.06 |
| Text + CFG joint | 0.04842–0.05659 | 21.9–28.1% | 71.4–75.7% | 12.62–15.71 |
| Text-only origin control | 0.06080–0.06581 | 18.8–28.1% | 70.9–72.6% | Untrained interpolation |
| Text + CFG origin control | 0.06123–0.06341 | 18.8–21.9% | 71.1–72.4% | Untrained interpolation |
| Frozen V6 comparator | 0.10272 | See matched report | Separate old origin head | Not provided |
| Mapped Pangram | 0.07705 | 0% observed | Not the A origin contract | Different span semantics |

The combined-minus-text involvement Brier difference, averaged over the three
fixed seeds before resampling source families, is **−0.00150** with a 95%
bootstrap interval **[−0.01052, +0.00600]**. It improves in two seeds and worsens
slightly in one. This does not establish a reliable incremental CFG benefit.
Combined-minus-CFG is −0.07667 [−0.10646, −0.04962]. These conditional intervals
do not account for Development selection or establish unseen-population quality.

Joint training improves involvement Brier versus the matched origin-only
controls in every seed: mean differences are −0.00991 for text and −0.00970
for combined, but their family intervals include zero. It also increases human
false positives at 0.5 versus those controls. The observed improvement therefore
does not qualify all operating points.

Conditional known-generator top-1 is 75.0–83.5% for text and 87.7–89.5% for
combined, versus 56.2–60.9% for CFG-only. Combined joint trails its attribution-only
control by 3.53 percentage points on average. Known-human top-1 remains weak:
28.1–37.5% for text and 34.4–37.5% for combined. Its joint-minus-attribution-only
difference is +1.04 points with a family interval spanning zero. This is evidence
of task tradeoffs, not guaranteed mutual benefit from a shared encoder.

Open-set decisions remain unreliable. Combined human known-profile fallback
frequency is 56.3–100%, with withheld rejection 62.5–81.3%. Generator known-profile
fallback is 71.3–93.6%, with withheld rejection 50–100%. Conditional rankings are
scored on documented contributors even when predicted involvement is low;
absent roles are not counted as asserted contributors. Independent four-writer
reference retrieval reaches only 21.9–28.1% top-1 for combined. There is no
independent generator-reference gallery, so generator retrieval is limited to
the classification-head ranking diagnostic.

Combined share MSE is 290.6–412.0 squared percentage points and bias is
−2.25 to −0.67 points. Endpoint examples dominate aggregate share MAE: the five
Development examples with realized share in `(0,25]` have MAE 31.4–39.9 points,
and the two in `(75,100)` have MAE 30.9–41.3. Good aggregate scores do not
establish partial-rewrite share quality. Combined involvement ECE is 0.067–0.081;
its three-way origin Brier (sum of three squared errors) is 0.347–0.379.

Exposure is material: 50 Development documents were used in V6 selection or
Calibration. Combined Brier is 0.01265–0.01978 there, but 0.10804–0.11793 on
the other 56 documents from six families. The corresponding text-only unmatched
range is 0.10339–0.11303: adding CFG does not improve that stratum in any seed.
Production/genre mixtures differ between strata, and unmatched rosters do not
exclude pretraining or alternate-edition exposure. Every combined-minus-Pangram
paired interval spans zero. Lower selected-Development Brier is not a parity
claim, particularly with high false positives and only 22 human documents.

The [aggregate evidence](a-v6-cfg-fusion-results.json) preserves all 21 runs,
calibration bins, share ranges, attribution/open-set metrics, task controls,
family uncertainty, exposure strata, runtime conformance and artifact hashes.

## Runtime and retained artifacts

CPU text extraction for 450 documents took **676.98 seconds**, including model
load and checks, and peaked at **1,103,292 KiB RSS** with zero swap. The frozen
V6 weights occupy 598,442,860 bytes. Cached CFG inputs reused prior measured
extraction; this run does not charge that work as zero. The prior 102-document
CFG extraction took 211.55 seconds and 426,864 KiB RSS, as documented in the
[baseline report](../grammar/crates/grammar-a-baseline/SUPERVISED_BASELINE.md).
These separate measurements are not a combined first-request latency benchmark.

The joint heads trained in approximately 1.1–1.2 seconds each after data/model
setup, with 83,284,480 bytes peak allocated CUDA memory. Jobs had an 8 GiB CPU
memory cap, zero swap, one CPU quota and a 20%-of-device PyTorch VRAM limit.
The complete pilot cgroup peaked at 1,483,493,376 bytes. GPU serving was not
started; unrelated services were preserved.

Representative combined seed 17 has a 738,651-byte PyTorch checkpoint and
5,551,947-byte numeric JSON artifact. Rust loads 1,469,016 bytes of float64
parameters; its warm numeric-head p50 is 0.0677 ms and p95 0.1303 ms. The replay
process peaks at 24,808 KiB RSS while loading all 106 Development requests and
reference predictions. All 21 Rust replays pass the existing 0.001 percentage
point tolerance, checking origin, share, involvement, named scores and fallback
names. The largest joint combined discrepancy is below 0.000024 points.

This qualifies the **numeric head only**. ModernBERT tokenization, attention,
rotary positions, GELU and normalization still need a Rust implementation and
serverless qualification. The complete pipeline is not yet a Workers model;
a compute-only Container remains a separately measured option. Workers AI
catalog/LoRA compatibility is not required. No deployment work was performed.

All checkpoints, numeric weights, contracts, reference predictions and recovery
states remain under ignored
`data/grammar-private/a-parity-stage2-20261026/fusion/`. Main entries are
`numeric-v1/{text,combined,cfg}`, `fit-v1/MODALITY/CONTROL-SEED/{model.pt,weights.json,fit.json}`,
`embeddings-v1/{manifest.json,rows.jsonl}`, and `summary-v1.json`.
Seed 17 is a reproducible example, not a winner chosen from the three seeds.
Fitted profiles and provider logs stay outside Git.

Build the named Rust examples in a capped build-host job, using the checkout's
own Cargo target. With the retained non-Test dataset and CFG export:

```sh
prepare_v6_fusion requests CFG_NUMERIC RECORDS DATA_MANIFEST NEW_REQUESTS
CPU_PYTHON grammar/crates/grammar-eval/python/v6_embeddings.py \
  --artifact QUALIFIED_V6 --requests NEW_REQUESTS/requests.jsonl --output NEW_EMBEDDINGS
prepare_v6_fusion join CFG_NUMERIC NEW_REQUESTS NEW_EMBEDDINGS NEW_NUMERIC
CUDA_PYTHON grammar/crates/grammar-eval/python/shared_a.py train \
  --data NEW_NUMERIC/combined --out NEW_FIT --seed 17 --epochs 250 \
  --device cuda --control joint --export-encodings
evaluate_supervised NEW_NUMERIC/combined NEW_FIT development NEW_EVALUATION.json
compare_supervised_pangram NEW_NUMERIC/combined NEW_FIT development \
  SAVED_DEV_PANGRAM SAVED_CAL_PANGRAM NEW_COMPARISON.json
shared_inference NEW_FIT/weights.json NEW_NUMERIC/combined/development.jsonl \
  NEW_FIT/development-predictions.json NEW_RUST_REPLAY.json
```

Repeat the declared modalities, controls and seeds before summarizing with
`summarize_v6_fusion NUMERIC_BUNDLE FIT_ROOT NEW_REPORT EXPOSURE_ROSTER`.
Use `shared_predict WEIGHTS.json NUMERIC_ROWS.jsonl NEW_PREDICTIONS.jsonl` for
streaming numeric inference; it never extracts text features. Input/output schemas remain the shared A v1
schemas. Every numeric dataset contract binds the full text representation,
Train normalization, original CFG contract, architecture, profiles, metadata
exclusion and file hashes. The preparation command deliberately supports this
frozen 450-row experiment; it is not an arbitrary-document deployment pipeline.

Proceed with new, family-separated populations, distinct withheld Calibration
and Test identities, balanced generators applied to comparable sources, more
independent writer works and fractional rewrites across positions/share ranges.
Preserve documented human-only histories separately from historical proxies.
Evaluate text-only and combined again under a frozen protocol with Pangram;
retain CFG-only as the hypothesis control. The current evidence supports that
comparison, while neither aggregate CFG failure nor this small fusion gain
settles whether ordered/tree CFG representations can add useful information.
