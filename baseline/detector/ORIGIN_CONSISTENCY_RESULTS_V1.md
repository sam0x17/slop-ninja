# Origin-consistency results

The consistency-trained candidate resisted the fixed private edit sequence
better than V6, but its human-text predictions and corpus detection losses
worsened. The experiment does not qualify a replacement reference detector.
The larger-backbone comparison has now started with the original data and
settings. V6 remains the reference for comparison.

Both four-epoch base fits and their public and private comparisons completed
on the Studio. The [protocol](ORIGIN_CONSISTENCY_V1.md) used 3,833 Train records
from 471 families. Both candidates received paired supervised exposures; only
the treatment added the fixed 0.5 Jensen-Shannon penalty. Development selected
epoch 2 for control and epoch 3 for consistency. Each model has 149,607,171
parameters. No evaluation text entered training, and no provider call was made.

The [public aggregate](results/origin-consistency-public-v1.json) retains all
seven corpus views, hosted model/profile slices, grammar diagnostics and paired
source-family uncertainty. A recorded assistant review of its 39 required
reports preceded the private comparison. The review is not independent human
annotation or fresh confirmation.

## Detection and human errors

Binary log loss below uses `P(model_only) + P(mixed)`. Lower is better. Mixed
rows are excluded from these binary summaries and remain in the separate
three-class metrics. The human-only and hosted-only rows each describe one
class, so neither alone measures overall detection performance.

| Evaluation | Source groups | V6 | Control | Consistency |
| --- | ---: | ---: | ---: | ---: |
| Primary R0 | 112 | 0.2035 | 0.2657 | 0.3333 |
| Primary R1 | 112 | 0.1033 | 0.1763 | 0.2488 |
| Primary R2 | 112 | 0.1007 | 0.1695 | 0.2327 |
| Phi-4 R0 | 104 | 0.1805 | 0.2399 | 0.3264 |
| Phi-4 R1 | 104 | 0.1663 | 0.2259 | 0.2886 |
| Phi-4 R2 | 104 | 0.1434 | 0.2037 | 0.2770 |
| All human roots | 117 | 0.1673 | 0.3090 | 0.4488 |
| Hosted model outputs | 24 | 0.4001 | 0.3698 | 0.1904 |

Both candidates have worse binary log loss than V6 on each corpus view.
Consistency is also worse than control on all seven. Each corresponding
paired 95% source-group bootstrap interval for the log-loss difference is
above zero. The stages reuse families, so these are correlated comparisons,
not seven independent replications. The hosted loss comparison is descriptive;
the corpus bootstrap intervals do not apply to it.

The nominal 1%/5% Calibration cutoffs are 0.843649/0.428737 for V6,
0.936632/0.862923 for control, and 0.958406/0.818938 for consistency. Their
observed counts at the stricter/looser cutoffs are:

| Observation | V6 | Control | Consistency |
| --- | ---: | ---: | ---: |
| Human false positives, out of 117 | 3 / 12 | 3 / 6 | 3 / 12 |
| Primary R0 model detections, out of 112 | 101 / 106 | 97 / 101 | 96 / 103 |
| Hosted model detections, out of 96 | 68 / 83 | 62 / 72 | 54 / 82 |

The different empirical cutoffs do not establish superiority at matched
population false-positive rates. Control's lower loose-cutoff human count
comes with reduced model sensitivity. Consistency improves hosted average
probabilities while performing worse on human roots and the corpus views.

Pangram flags 85 hosted texts under the existing rule of at least 10% nonhuman
content. Among those, A probabilities below 10% occur in zero V6 cases, three
control cases and two consistency cases. The three control passages were read
in full; they come from two source families. Generator and profile differences
remain substantial. Each hosted profile contains only four source groups;
these slices do not establish broad style or generator generalization.

Both candidates detect all 16 model-origin grammar states and flag none of ten
human states. Those 26 states come from three groups, include existing editorial
reservations, and did not reproduce the known private failure. Their perfect
counts provide no general accuracy estimate.

## Fixed private full-document comparison

All three source versions remained byte-identical to the earlier study.
The runner scored all five planned windows for each candidate/version and
verified complete coverage. These numbers are the direct whole-document
observations, not window minima or averages:

| Version | V6 A probability | Control A probability | Consistency A probability | Saved Pangram flagged content |
| --- | ---: | ---: | ---: | ---: |
| Original | 90.39% | 80.81% | 97.14% | 100.00% |
| Repair bundle | 77.23% | 51.89% | 93.56% | 100.00% |
| Repairs plus joins | 51.89% | 18.55% | 87.15% | 96.30% |

The original-to-final decrease is 38.50 percentage points for V6, 62.26 for
control and 9.99 for consistency, against 3.70 points in saved Pangram flagged
content. Consistency retains all three texts above its looser cutoff; only the
original remains above its stricter cutoff. Control flags none at either cutoff.

This is evidence of reduced brittleness on one known source family. These edits
were already selected against V6; the three versions are not independent samples.
Pangram's content fractions and A's origin probabilities are different quantities.
Keep the existing editorial reservations. The private gain cannot override the
human errors or justify optimizing new B outputs against this candidate yet.
Exact text, identifying bindings, detailed scores and invocation logs remain in private
Studio storage outside repositories and datasets.

## Revision stability

The additional [Rust diagnostic](examples/revision_score_stability.rs) reads the
completed public reports and matches the same families across R0/R1/R2 and all
three detectors. It makes no model call. This analysis was added after inspecting
public results, and is explicitly exploratory.

The [saved results](results/revision-score-stability-v1.json) give mean ranges
of the combined A probability across the three stages:

| Cohort | V6 | Control | Consistency |
| --- | ---: | ---: | ---: |
| Primary, percentage points | 8.03 | 7.26 | 6.54 |
| Phi-4, percentage points | 5.18 | 4.07 | 5.19 |

Three-class calibrated Jensen-Shannon divergence increases for consistency
against control in both cohorts: 0.08559 to 0.09642 nats in primary, and
0.02138 to 0.03957 in Phi-4. Temperatures and selected epochs differ; this is
not a direct measurement of the uncalibrated training penalty. Stable wrong
predictions can have zero variation, so stability must accompany accuracy and
human-error evidence. These results show no general invariance gain.

Studio formatting, all-target tests, Clippy with warnings denied and the new
example build passed. The [completion record](results/origin-base-comparison-completion-v1.json)
binds the public aggregate and stability result and confirms all 30 private
window observations. The whole-document runner completed its first execution
after the public review. The 395,834,371-parameter control/consistency pair now
tests the [predeclared larger-backbone recipe](CAPACITY_LARGE_V1.md). Its results,
broader data coverage and fresh confirmation remain outstanding.

The subsequent [human-error analysis](ORIGIN_ERROR_RESULTS_V1.md) finds that
both new candidates still regress at temperature one and lose pooled ranking
performance. Within-family ordering is already near perfect. Human loss worsens
most in historical summaries and news; manual inspection retains source-view
and register observations without changing the proxy labels or admitting Test
examples to training.

The [saved epoch histories](results/base-fit-history-v1.json) show falling online
training loss through epoch four, while binary Development loss rises from the
selected epoch to the last epoch: 0.1219 to 0.2396 for V6, 0.1713 to 0.2701 for
control, and 0.1971 to 0.3346 for consistency. Online weighted losses with dropout
cannot establish the saved models' per-class Train fit. The
[Train-fit diagnostic](TRAIN_FIT_DIAGNOSTIC_V1.md) now scores the full original
Train archive in both existing formatting views, with original artifacts and
cutoffs. Its [preparation record](results/train-fit-preparation-v1.json) binds
the checked runner and 22,998 planned observations; results remain pending.
