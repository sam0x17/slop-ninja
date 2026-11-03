# Narrative detector results

Measured 2026-09-12. V5 reduced human/model binary Test log loss from 0.6970 to
0.1430 across 25 historical narrative source families. At both frozen operating
points, human false positives fell from 5/25 to 1/25 while detection stayed at
25/25 fully model-written drafts.

The separate Phi-4 Test also improved binary log loss, from 0.6523 to 0.1422,
but exposed a sensitivity tradeoff. At the stricter operating point, v5 detected
19/21 drafts versus v4's 21/21, with human false positives falling from 4/21 to
1/21. The two missed drafts used Anti-AI and Fix Slop instructions. This remains
an experimental detector with an unresolved weakness on the main style-steering
target. No new Pangram calls or Pangram-derived training labels were used.

Download the v5 weights and CPU inference bundle (omitted from the public copy).
The 558,321,008-byte archive has SHA256
`78a24a7d7908257eb1e9182c0d18f6aa350f630fe30f654c020b2a9fad4ec482`;
GitHub's asset digest matches the verified local archive. Fine-tuned weights use
the project's CC BY-SA 4.0 release policy, with upstream notices and full source
credits included.

The [aggregate results](results/encoder-narrative-v5.json) include both training
histories, all six evaluations, provider/profile slices, three paired comparisons,
thresholds and recovery provenance.

## Data and selection

The [frozen v5 protocol](ENCODER_NARRATIVE_V5.md) and its
[target amendment](ENCODER_NARRATIVE_V5_TARGET.md) specify the experiment.
The [corpus report](NARRATIVE_CORPUS_RESULTS.md) records every generation
attempt, admission failure and retained denominator for Qwen, Mistral, OLMo
and Phi-4. The 289 admitted CMU human roots are historical proxies; their
publication history does not establish verified contemporary human workflows.

Train contains 2,925 rows from 471 families, Development 183/61, Calibration
201/67 and primary Test 75/25. Train retains all available complete provider
pairs with one human root per family. Equal total loss weight per family/origin
prevents extra provider outputs from increasing a source's contribution.
Development, Calibration and primary Test each use one provider pair per family,
chosen by the frozen hash rule before detector scoring. Every admitted row fits
the 1,024-token contract without truncation or filtering.

Phi-4 was absent from fitting, Development and Calibration. Its separate Test
contains 63 rows from 21 families. All 21 families and human records also occur
in primary Test, so these are correlated comparisons with 25 distinct human
sources overall. They must not be pooled as 46 independent human examples.
Generation failures also change which planned families survive admission.

Both ModernBERT-base fits used four epochs, seed 17, batch four, FP32 MPS,
AdamW, weight decay 0.01 and gradient clipping at 1.0. Raw and collapsed-whitespace
Train exposures alternated by family and epoch. Three-class training remained
unchanged; selection used uncalibrated human/model binary Development log loss
with `P(AI) = P(model_only) + P(mixed)`. Mixed examples were auxiliary and excluded
from that selection metric. All eight epochs passed the nonzero binary-recall gate.

| Epoch | Learning rate 0.0000075 | Learning rate 0.00001 |
| --- | ---: | ---: |
| 1 | 0.592340 | 0.321477 |
| 2 | 0.199872 | 0.272624 |
| 3 | 0.357288 | 0.425298 |
| 4 | 0.244960 | 0.239247 |

The first run's epoch two won. The second run's epoch four had better binary
accuracy and Brier score on Development, but those metrics could not replace the
declared log-loss objective. Earlier epochs break within-run ties; the lower
learning rate breaks equal-loss ties between runs. Calibration and Test did not
enter checkpoint choice. Corpus coverage, learning rates, epoch budget and the
selection objective changed together relative to v4; this experiment cannot
attribute the gain to one of them.

## Packaging recovery

The first fit completed all four epochs and saved its selected classifier,
then failed while copying a missing `requirements.lock` from its frozen runner.
The original failed directory and log remain unchanged. A new runner snapshot
adds the verified lock; all nine Python files match the original frozen bytes.
The lock matches the installed reference package versions.

Recovery loaded the saved classifier, repeated only the prescribed three-class
Calibration temperature fit, and packaged a new artifact. Every classifier and
tokenizer file retains its original bytes; serialized weight tensors also matched
exactly. Recovery performed no optimizer steps and no Test inference. Both final
bundles passed their bundled CPU reference checks before selection was frozen.
The selected scalar temperature is 1.4049475905635938.

The original in-memory Calibration values, full runtime inventory and total
training elapsed time were lost before persistence. Recovery metadata marks
those gaps explicitly. Epoch identity follows the frozen save/reload code,
completed log and failure location; no original per-epoch weight hash exists.
The second rate fitted normally using the same frozen code and supplied lock.
Future training now checks packaging inputs before loading a model and preserves
training/Calibration metadata before copying runner files.

## Binary comparison

The following log losses and Brier scores use frozen calibrated CPU probabilities
and exclude mixed examples. Detection counts use each artifact's original
Calibration thresholds. The nominal 1% and 5% targets are not measured population
false-positive guarantees.

| Partition | Model | Binary rows | Log loss | Brier | Human false positives, 1% / 5% | Model drafts detected, 1% / 5% |
| --- | --- | ---: | ---: | ---: | --- | --- |
| Development | v4 | 122 | 0.3941 | 0.1227 | 9/61 / 9/61 | 57/61 / 57/61 |
| Development | v5 | 122 | 0.1900 | 0.0595 | 1/61 / 2/61 | 58/61 / 58/61 |
| Primary Test | v4 | 50 | 0.6970 | 0.2245 | 5/25 / 5/25 | 25/25 / 25/25 |
| Primary Test | v5 | 50 | 0.1430 | 0.0444 | 1/25 / 1/25 | 25/25 / 25/25 |
| Phi-4 Test | v4 | 42 | 0.6523 | 0.2114 | 4/21 / 4/21 | 21/21 / 21/21 |
| Phi-4 Test | v5 | 42 | 0.1422 | 0.0406 | 1/21 / 1/21 | 19/21 / 20/21 |

The paired calculation uses 512 deterministic resamples of complete source
families. Candidate-minus-v4 binary log loss is -0.5540 on primary Test, with a
95% percentile interval of -0.7567 to -0.3582. On Phi-4 it is -0.5101, interval
-0.7428 to -0.3077. Binary Brier changes are -0.1801, interval -0.2486 to -0.1111,
and -0.1708, interval -0.2396 to -0.1036, respectively. These intervals assume
independent families within each view and exclude pretraining overlap and new
registers. Development intervals are descriptive because Development selected
the candidate.

V5 uses strict cutoffs of 0.9052925262799454 and 0.8884499969916239 for the two
targets, producing 0/67 and 3/67 human Calibration errors. V4 retains
0.9055049212676647 and 0.902395786182547, with 0/34 and 1/34 Calibration errors.
No compared Test decision lies within the diagnostic cutoff margin of 0.000002.
Different Calibration populations and small counts prevent a claim of superiority
at a matched population false-positive rate.

## Writing-profile results

Each profile includes its paired human roots. Counts below use the stricter
operating point; both primary-Test settings produce identical counts.

| Test view | Instruction | v4 drafts detected | v5 drafts detected | v4 human false positives | v5 human false positives |
| --- | --- | ---: | ---: | ---: | ---: |
| Primary | Anti-AI | 4/4 | 4/4 | 0/4 | 0/4 |
| Primary | Fix Slop | 4/4 | 4/4 | 2/4 | 0/4 |
| Primary | Direct | 4/4 | 4/4 | 1/4 | 1/4 |
| Primary | Informal | 5/5 | 5/5 | 1/5 | 0/5 |
| Primary | Plain | 4/4 | 4/4 | 1/4 | 0/4 |
| Primary | Source matched | 4/4 | 4/4 | 0/4 | 0/4 |
| Phi-4 | Anti-AI | 4/4 | 3/4 | 0/4 | 0/4 |
| Phi-4 | Fix Slop | 3/3 | 2/3 | 1/3 | 0/3 |
| Phi-4 | Direct | 4/4 | 4/4 | 1/4 | 1/4 |
| Phi-4 | Informal | 5/5 | 5/5 | 1/5 | 0/5 |
| Phi-4 | Plain | 2/2 | 2/2 | 1/2 | 0/2 |
| Phi-4 | Source matched | 3/3 | 3/3 | 0/3 | 0/3 |

At the nominal 5% setting, the missed Phi-4 Anti-AI draft is detected; the Fix
Slop miss remains. V4 detects every draft in both views at its frozen settings.
V5's improved pooled loss therefore does not establish stronger resistance to
deliberate style steering. These slices contain only two to five families, and
the prompts establish attempted steering rather than verified removal of model
style or preservation of meaning. The aggregate also retains Development,
collection and generator/profile slices.

## Three-class diagnostics and remaining work

Three-class argmax measures a different decision from thresholded binary
detection. Primary-Test accuracy rose from 53/75 to 58/75, with log loss falling
from 0.8854 to 0.5159. Phi-4 accuracy rose from 32/63 to 40/63 and log loss fell
from 0.9037 to 0.5972. The primary accuracy-difference interval includes zero;
the Phi-4 interval reaches zero.

The model-only class became less distinct from mixed: correct model-only argmax
predictions fell from 24/25 to 13/25 on primary Test and from 18/21 to 7/21 on
Phi-4. Correct mixed predictions rose from 16/25 to 23/25 and from 2/21 to 14/21.
At the strict binary cutoff, auxiliary edit sensitivity stayed at 24/25 on
primary Test and fell from 20/21 to 16/21 on Phi-4. These recorded edits of
historical proxies do not establish performance on genuine human/model
collaboration.

The selected artifact is
`da3a604d7bd1708f5ba1c4a4a4be946d77670492b702e41e267fb86237dfb8c9`.
The two-rate selection record has SHA256
`70fe91d23f89d0673a6e0bdea84e44fc641394cc79ba3f7d54a26429448cec74`.
Selection, thresholds, corpus inputs and evaluator hashes were frozen before
Test opening at 2026-09-12 04:31:57 UTC. The release includes the selected artifact,
aggregate report and provenance. Corpus passages, individual predictions,
generation responses and original recovery logs remain in ignored storage.

V5 is the next research candidate for binary origin detection. It has neither
Pangram parity nor subnet qualification. The opened Test families are retired
from fresh confirmation. The next experiment should emphasize fully
model-written prose under stronger and repeated style revision, with new
confirmation families and an unseen generator. Model-to-model revisions retain
model-only lineage. Author matching, meaning preservation and target tone need
independent measurements; origin probabilities cannot certify those properties.
