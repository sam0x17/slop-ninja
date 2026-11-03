# V5 against Pangram on shared Development texts

Measured 2026-09-12 on the Studio. V5 detected 32 of 38 model-only or mixed
texts at its stricter threshold and 34 at its looser threshold, with one false
positive among 19 historical human proxies. Pangram detected all 38 with no
human flags. V5 improves on v2's 20 detections but still trails Pangram on this
shared cohort.

| Detector and setting | Human flagged / 19 | Model-only detected / 19 | Mixed detected / 19 |
| --- | ---: | ---: | ---: |
| Pangram 4.0, AI plus assisted fraction >= 0.10 | 0 | 19 | 19 |
| V2, original frozen threshold | 0 | 12 | 8 |
| V5, stricter original threshold | 1 | 17 | 15 |
| V5, looser original threshold | 1 | 17 | 17 |

This follows the [declared comparison](PANGRAM_CORPUS_EXPANSION.md) and reuses
the 57 first-pass provider annotations from the [earlier Pangram study](PANGRAM_CORPUS_RESULTS.md).
No new Pangram call, model fit, checkpoint selection or calibration was performed.
Both v5 thresholds retain their original values, 0.9052925262799454 and
0.8884499969916239, with strict greater-than comparison. Their nominal 1% and
5% Calibration targets do not certify population false-positive rates.

All 57 records come from the same 19 source families. Those families remain in
v5 Development and have no family or exact-text overlap with its Train corpus.
Development has already influenced selection; this is a descriptive comparison,
not fresh confirmation. Pangram and v5 use different frozen operating rules.
The observed counts do not establish performance at a matched population
false-positive rate or justify switching the subnet's quality anchor.

## Remaining failures

Both missed model-only texts were Mistral drafts of PLOS sources: one used
informal instructions and the other plain instructions. V5 detected all ten
Qwen drafts and seven of nine Mistral drafts. Its human false positive came
from Wikinews. At the looser threshold, both missed mixed texts also belonged
to the Mistral informal/plain slices. Every slice contains very few families.

V5 detected all four Anti-AI drafts and all four Fix Slop drafts in this cohort.
At the stricter threshold it missed one mixed text in each of those profiles;
the looser threshold detected both. This does not contradict the separate
Phi-4 Test misses: these are different texts and generator routes.

Three-class argmax accuracy was 42/57, compared with v2's 33/57. V5 correctly
assigned 16/19 human, 11/19 model-only and 15/19 mixed records to their exact
origin classes. Binary human-versus-model log loss was 0.332982, excluding mixed
records. Pangram content fractions were not treated as origin probabilities.

## Reproduction and expansion

The released v5 artifact and bundled CPU inference were unchanged. The remote
evaluation exited successfully at 18:37:42 UTC after exporting all 57 responses.
Protocol, input, annotation, threshold and executable bindings matched after
the run. No v6 data or scores entered this comparison.

The [aggregate results](results/pangram-v5-development-v1.json) retain both
operating points, family-bootstrap uncertainty, three-class diagnostics and all
collection, generator and writing-profile slices. Per-record predictions and
provider responses remain ignored.

- Full private evaluation report SHA256:
  `40f490658b43ade8b70101e4628234e3c0b698a3eb23cd42db0ff96d7a3927fd`.
- Public aggregate SHA256:
  `18fab151664c0885efdcd60c72c32dc33a1e628358a2f40d1bb10002ca158002`.
- Original comparison declaration SHA256:
  `935c9343280f80a144c74c1ed5930d4aa09451547d36f52e48469709baa2112b`.

A separate annotation plan covers 2,925 unique Train texts from 471 source
families: 471 human proxies, 1,227 model drafts and 1,227 model edits. The
posted-price estimate is $292.04. The [revised collection plan](PANGRAM_DATASET_V2.md)
replaced submission of this full plan. Keep provenance labels and provider annotations separate,
and reserve source-disjoint data for later comparisons.
