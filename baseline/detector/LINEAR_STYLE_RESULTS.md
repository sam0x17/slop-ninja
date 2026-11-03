# Word and grammar probe results

The best combined model recovered mixed-origin predictions on the style-diverse
Development set. It correctly classified 35 of 57 rows, including 10 of the 19
mixed examples. The v2 encoder classified 33 rows correctly and no mixed
examples, while retaining slightly better probability loss. These development
results justify fresh comparison of explicit features; they do not justify
replacing the reference detector.

## Frozen comparison

The [protocol](LINEAR_STYLE_PROBE.md) and Rust runner were committed as
`6e71ae1ce386a25194b8322de7017450a04385ae` before fitting. Six candidates used
291 Training rows from 97 source families, 57 Development rows from 19 families,
and 42 Calibration rows from 14 families. No Test features or predictions were
opened. Candidate choice used uncalibrated Development log loss with the declared
minimal class-recall gate. Every candidate passed that gate; the unrestricted
loss winner was the same combined model.

| Features | Learning rate | Selected epoch | Development loss | Correct / 57 | Mixed correct / 19 |
| --- | ---: | ---: | ---: | ---: | ---: |
| Word | 0.0003 | 29 | 0.948438 | 31 | 7 |
| Grammar | 0.0003 | 11 | 0.987760 | 29 | 4 |
| Combined | 0.0003 | 10 | 0.950626 | 30 | 4 |
| Word | 0.001 | 12 | 0.937040 | 31 | 7 |
| Grammar | 0.001 | 7 | 0.978050 | 29 | 5 |
| Combined | 0.001 | 4 | 0.899395 | 35 | 10 |
| Previously selected v2 encoder | 0.00002 | 2 | 0.891927 | 33 | 0 |

Each standalone model has 8,192 coordinates. The combined model has 16,384,
preserving both standalone coordinate lists, document frequencies and scales
exactly. The runner verified these identities for both learning rates. Grammar
therefore adds features without displacing lexical features. The comparison also
adds parameters; it does not isolate grammar from increased model capacity.

For the selected combined model, the confusion matrix is:

| Actual / predicted | Human only | Model only | Mixed |
| --- | ---: | ---: | ---: |
| Human only | 17 | 0 | 2 |
| Model only | 4 | 8 | 7 |
| Mixed | 4 | 5 | 10 |

The encoder recognized 16/19 model-only examples, compared with 8/19 here. Mixed
recovery comes with a model-only classification tradeoff. These are argmax origin
decisions; human error counts are not false-positive rates at a calibrated binary
operating point. Calibration outputs were retained but did not select a candidate.

## Reproduction and limits

The [machine-readable result](results/linear-style-probe-v3.json) records all six
candidates, artifact hashes and coordinate identities. Its
[frozen run plan](results/linear-style-probe-v3-plan.json) binds the feature cache,
protocol and executables. Exact data, complete epoch
histories, calibrated artifacts, invocation logs and frozen inputs are local in
`data/baseline-detector/linear-style-probe-v3/`. The feature cache hash is
`e2ff10f9c53a81333e6227aafdda3a93b962ebb33a313e469827e91ed01aa33c`.

Smaller learning rates avoided the uniform-checkpoint failure of the earlier
grammar fits. Across the two corpora that is development evidence, not a controlled
estimate of a learning-rate effect: corpus and coordinate allocation also changed.
The 19 Development families are too few to claim a reliable performance ordering,
and all human/mixed labels remain historical proxies. Both generators were seen
during training. None of these results measures Pangram parity or author matching.

Keep the combined model as a candidate for a new frozen comparison. Prioritize
broader independently labeled data and a documented mixed-workflow set before
expanding the feature search. The [standard-corpus plan](STANDARD_CORPUS.md)
adds reusable external annotations while preserving independent origin evidence.
