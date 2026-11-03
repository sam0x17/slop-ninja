# Neural feature heads: first fixed grid

All eleven fits completed on Studio macOS arm64 CPU. The
[grid audit](results/feature-grid-summary-v1.json) checked the frozen inputs,
artifact files, complete histories and earliest eligible Development minima.
These results use one seed and the fixed [training recipe](FEATURE_NN_V1.md).

The selection metric is uncalibrated binary negative log likelihood on the
same R2 Development human/model view: 212 records from 106 source families.
Mixed records remain auxiliary training examples and have separate three-class
metrics. Lower loss is better.

| Representation | Hidden width | Parameters | Selected epoch | Development loss |
| --- | ---: | ---: | ---: | ---: |
| Word | linear | 24,579 | 7 | 0.2775 |
| Word | 128 | 1,049,091 | 2 | 0.1989 |
| Word | 256 | 2,098,179 | 1 | 0.2279 |
| Word | 512 | 4,196,355 | 1 | 0.2030 |
| Word | 1,024 | 8,392,707 | 1 | 0.1917 |
| Grammar | linear | 24,579 | 4 | 0.3756 |
| Grammar | 128 | 1,049,091 | 1 | 0.3730 |
| Grammar | 512 | 4,196,355 | 1 | 0.3590 |
| Combined | linear | 49,155 | 3 | 0.2639 |
| Combined | 128 | 2,097,667 | 1 | 0.1978 |
| Combined | 512 | 8,390,659 | 1 | 0.1714 |

The combined 512-unit head improved Development loss over the similarly sized
word-only 1,024-unit head. The combined 128-unit head also improved on the word
256-unit parameter control. Grammar features therefore merit further testing
alongside word features under this recipe. The grammar-only candidates were
weaker. These are Development comparisons used for selection, with no
multi-seed ordering or external generalization claim.

Every candidate fit Train closely by epoch 30. Most nonlinear heads reached
their best Development epoch after the first pass. For combined-512, binary
Train loss fell from 0.03965 at the selected epoch to 0.00000363 at epoch 30,
while Development loss rose from 0.17137 to 0.45982. This corpus does not expose
a failure of these heads to fit their training examples. More capacity alone
has not addressed generalization.

The existing V6 encoder had lower selected Development loss, 0.12185, on the
same source-shard hashes and evaluation views. It used a pretrained encoder,
different optimization and whitespace augmentation. This is a comparison of
the resulting systems, not an isolated architecture ablation. The feature-only
results do not justify replacing V6.

The [diagnostic comparison](results/feature-diagnostics-summary-v1.json) is now
complete under the frozen [protocol](FEATURE_DIAGNOSTICS_V1.md). All eleven
Calibration operating points were fixed before diagnostic inference. The
206-file postflight binding check passed. Development selection still identifies
word-1024, grammar-512 and combined-512; diagnostic results did not change the
selection or thresholds.

The feature heads generalized poorly to the 96 saved hosted outputs, which
cover four requested models and six writing profiles. Counts below use each
model's nominal 1% and 5% Calibration human false-positive operating points.
These are empirical thresholds from 108 human Calibration families, not
population guarantees.

| Candidate | Flagged at 1% point | Flagged at 5% point |
| --- | ---: | ---: |
| Word linear | 33/96 | 45/96 |
| Word 128 | 26/96 | 46/96 |
| Word 256 | 33/96 | 48/96 |
| Word 512 | 15/96 | 41/96 |
| Word 1,024 | 20/96 | 42/96 |
| Grammar linear | 12/96 | 42/96 |
| Grammar 128 | 9/96 | 38/96 |
| Grammar 512 | 10/96 | 25/96 |
| Combined linear | 29/96 | 47/96 |
| Combined 128 | 14/96 | 56/96 |
| Combined 512 | 8/96 | 54/96 |
| Existing V6 encoder | 68/96 | 83/96 |

Saved Pangram observations flagged 85/96 at the existing diagnostic rule,
AI plus AI-assisted fraction at least 0.1. That rule is not matched to the
classifiers' false-positive rates. The hosted texts come from 24 Train roots
and contain no independent human controls; these counts cannot estimate general
accuracy or human false positives. Nevertheless, every feature candidate missed
more of these known model outputs than V6 at both operating points. No feature
head qualifies to replace the encoder or provide trusted guidance to B.

The local gradient calculation worked on the reviewed grammar diagnostic:
combined-512 agreed with the actual margin-change direction for all 17 edits
and with all 27 within-source pair orderings. Its mean absolute margin error
was 0.00944. A [fresh replay](results/feature-gradient-replay-v1.json) recomputed
the largest estimated change for each Development-selected representation and
checked the report against actual model execution. These are nine controls and
17 edits across three known Development families. Some edits have recorded
quality concerns, and none has a Pangram observation. The arithmetic checks
establish neither external gradient transfer nor meaning preservation.

Further work should retain the text encoder and address training diversity and
robustness. Grammar features remain candidates for a controlled fusion
experiment. Simply increasing these feature heads' capacity did not resolve
their generalization failure. The frozen snapshots remain available on the
Studio; this comparison made no paid calls and promoted no reference model.
