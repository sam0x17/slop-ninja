# Expanded detector results

Measured 2026-09-11. The v4 encoder improved three-class accuracy from 27/51 to
40/51 on fresh Test, reduced log loss from 0.8803 to 0.5282, and recovered mixed
predictions. At its frozen detection thresholds, it flagged 31/34 model-or-assisted
texts with 0/17 human-proxy false positives. The prior v2 encoder flagged 9/34
with the same observed false-positive count. These are 17 historical encyclopedia
source families, not evidence of a reliable population false-positive bound.

The candidate remains experimental. On the previously scored Development texts,
it still trails Pangram: 29/38 detections versus 38/38. No additional Pangram
calls or provider-derived training targets were used in this experiment.

Download the v4 model bundle (omitted from the public copy).
It includes weights, the CPU runner, source credits and all aggregate comparisons.
The uploaded 557,435,341-byte archive has SHA256
`c1a2093ebf3ca669db4b0f3cbf497569c7aaf724ec45b6ae8fe9c85d5f7eca2b`;
GitHub's asset digest matches the locally verified archive.

## Data and selection

The [frozen experiment](ENCODER_EXPANSION_V4.md) changed corpus coverage and the
training objective together. It cannot isolate which change caused the gain.
The [generation report](CORPUS_EXPANSION_V4.md) records all attempted calls,
admission failures and final hashes. Train has 1,633 rows from 275 source families,
Development 105/35, Calibration 102/34 and fresh Test 51/17. No row was truncated
or removed by the 1,024-token audit.

Train includes all complete Qwen, Mistral and OLMo pairs available for a family,
with one human root. Equal family-origin loss mass prevents extra provider pairs
from increasing a source's total contribution. Evaluation uses one provider's
pair per family, selected by the prespecified hash rule before scoring. The
generator mix is not an unseen-generator test. Historical publication supplies
weak human-origin evidence; recorded model copyedits supply weak mixed labels.

Both ModernBERT-base fits used five epochs, seed 17, batch four, FP32 MPS,
AdamW, weight decay 0.01, gradient clipping at 1.0 and alternating raw/whitespace
Train views. Selection required positive Development recall for all three
classes, then minimum uncalibrated log loss. Test and Calibration values did
not enter selection.

| Learning rate | Selected epoch | Development log loss | Correct | Mixed recall |
| --- | ---: | ---: | ---: | ---: |
| 0.00001 | 4 | 0.585122 | 77/105 | 18/35 |
| 0.00002 | 2 | 0.683175 | 70/105 | 16/35 |

The first fit won. Epoch five worsened log loss to 1.347855 and 2.233873,
respectively. Both full histories and candidate identities are retained in the
[aggregate results](results/encoder-expansion-v4.json). Fitting and packaging
the first candidate took 650.8 seconds on the M5 Max. A separate synthetic
[attention benchmark](ml/TRAINING_ATTENTION_BENCHMARK.md) rejected its SDPA
prototype; no attention change entered these fits.

After selection, the winner's scalar Calibration temperature was 2.585710.
All comparisons below use each artifact's frozen calibrated CPU probabilities.
The original v2 Calibration export preserves its original strict threshold;
replaying Calibration on another host can move a boundary despite meeting the
reference-vector tolerance.

## Same-corpus comparison

Accuracy and mixed recall use three-class argmax. Detection counts instead use
`P(model_only) + P(mixed)` at the frozen operating threshold. These decision
rules can disagree on an individual text.

| Partition | Encoder | Correct | Log loss | Brier | Mixed recall | Model-or-assisted detections | Human-proxy false positives |
| --- | --- | ---: | ---: | ---: | ---: | ---: | ---: |
| New Development | v2 | 60/105 | 0.8058 | 0.4973 | 0/35 | 33/70 | 0/35 |
| New Development | v4 | 77/105 | 0.5417 | 0.3453 | 18/35 | 60/70 | 0/35 |
| Fresh Test | v2 | 27/51 | 0.8803 | 0.5376 | 0/17 | 9/34 | 0/17 |
| Fresh Test | v4 | 40/51 | 0.5282 | 0.3378 | 12/17 | 31/34 | 0/17 |

The paired source-family bootstrap gives a Test accuracy gain of 25.49 percentage
points, with a 95% percentile interval of 13.73 to 37.25 points. The paired log-loss
change is -0.3520, interval -0.5309 to -0.2084; Brier change is -0.1998, interval
-0.3040 to -0.1101. The calculation uses 512 deterministic resamples of complete
families. It assumes independent families and does not cover pretraining overlap,
unseen registers or uncertain origin labels. Development intervals are descriptive
because Development selected the model.

The v4 Test confusion matrix, with true classes in rows and predictions in
columns ordered human/model/mixed, is:

```text
15  1  1
 0 13  4
 0  5 12
```

The frozen v4 thresholds are strictly greater than 0.9055049212676647 for the
nominal 1% target and 0.902395786182547 for 5%. They produce 0/34 and 1/34 human
Calibration errors. V2 retains 0.8935751794569573 for both targets, fitted on only
14 human Calibration examples. Both v4 operating points produce the same Test
counts. None of the compared decisions lies within the diagnostic cutoff margin
of 0.000002. Different thresholds and small samples prevent a matched-population-FPR
superiority claim. Zero observed false positives does not mean zero risk.

## Generator slices and the Pangram gap

Every Test provider slice improved log loss. Each slice includes its paired
human roots; the denominators are too small to rank generators reliably.

| Generator | Families | v2 correct | v4 correct | v2 log loss | v4 log loss | v2 detections | v4 detections |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| Mistral | 7 | 12/21 | 17/21 | 0.7858 | 0.4672 | 1/14 | 13/14 |
| OLMo | 3 | 4/9 | 7/9 | 1.1063 | 0.5747 | 3/6 | 6/6 |
| Qwen | 7 | 11/21 | 16/21 | 0.8778 | 0.5693 | 5/14 | 12/14 |

The previously purchased [Pangram observations](PANGRAM_CORPUS_RESULTS.md) cover
57 old Development texts from 19 families. On those exact texts, v4 improved
accuracy from 33/57 to 41/57, log loss from 0.8321 to 0.5882, and detections from
20/38 to 29/38. Pangram flagged all 38 using its recorded document-fraction rule;
all three systems flagged none of the 19 human proxies. This is an adaptive
Development diagnostic, with distinct score definitions and operating points.
It establishes neither parity nor independent confirmation against Pangram.

## Artifact and remaining work

The selected artifact is
`101445a73fa1e44b996a72e6536e8fa082f76ade415faf978c375b3c3f8429b7`.
Selection was frozen before Test under SHA256
`59361c47527c4c89bb925d9841a71402183217f9380be011cb6e6c0f21eda8b2`.
Training used the pinned runtime from commit `6738281`; evaluation and paired
comparison use `8e8a1ab`. Bundled CPU inference verified file hashes, dependency
versions and synthetic reference vectors before all six evaluations. Each
comparison preserves exact input/output bindings locally.

Fine-tuned weights use the project's CC BY-SA 4.0 release policy and include
`DATA_RIGHTS.json`; the upstream Apache 2.0 notice remains separate. Source text,
individual predictions and generation responses stay in ignored local storage.
The public aggregate retains all six evaluations, both training histories and
the three paired comparisons. Reproducible inference is supported; reconstructing
the complete generated corpus requires the local archives.

This improves the experimental detector but does not qualify a subnet reference.
Personal and narrative prose, genuine human/model collaboration, other languages
and unseen generators still need evaluation. Fidelity was requested during
generation but was not independently certified. Scores describe document-origin
classes, not AI-written word fractions. The opened Test families are now retired
from fresh confirmation; further model choices need a new confirmation cohort.
