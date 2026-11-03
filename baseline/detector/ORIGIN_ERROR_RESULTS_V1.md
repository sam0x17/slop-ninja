# Human errors and ranking after origin-consistency training

The new base candidates lose discrimination as well as assigning worse human
probabilities. Removing their temperature calibration does not repair either
problem. V6 remains the reference; this analysis does not qualify a replacement.

The [Rust diagnostic](examples/origin_error_anatomy.rs) follows the
[analysis protocol](ORIGIN_ERROR_ANALYSIS_V1.md). It reads the same seven completed
Test views for V6, control and consistency, with no inference or provider call.
The [results](results/origin-error-anatomy-v1.json) retain every case, original
operating point, per-human observation and input binding. These are exploratory
diagnostics of already inspected Test results. Repeated stages share families.

## Calibration does not explain the regression

Temperature-one probabilities are reconstructed from each saved three-class
probability vector using its artifact's original temperature. No new temperature
or threshold is fitted. On every complete case, all three models have lower
binary log loss with their deployed calibration than at temperature one. Both
new candidates still have worse loss than V6 on all seven cases at temperature
one, with consistency worse than control throughout.

For the 117 human roots, mean log loss is:

| Scores | V6 | Control | Consistency |
| --- | ---: | ---: | ---: |
| Deployed calibration | 0.1673 | 0.3090 | 0.4488 |
| Temperature one | 0.1946 | 0.3450 | 0.6561 |

This rules out removing the existing temperature as a fix. It does not establish
that every possible recalibration would fail. Reconstructing probabilities also
retains the precision limits of the saved scores.

## Ranking and separation within source families

AUROC compares model-only texts with human roots across the whole case, excluding
mixed-origin rows. Higher is better. Each case contains one human and one
model-only observation per source family.

| Case | Families | V6 | Control | Consistency |
| --- | ---: | ---: | ---: | ---: |
| Primary R0 | 112 | 0.9832 | 0.9723 | 0.9685 |
| Primary R1 | 112 | 0.9980 | 0.9959 | 0.9947 |
| Primary R2 | 112 | 0.9973 | 0.9959 | 0.9945 |
| Phi-4 R0 | 104 | 0.9846 | 0.9793 | 0.9688 |
| Phi-4 R1 | 104 | 0.9867 | 0.9826 | 0.9753 |
| Phi-4 R2 | 104 | 0.9904 | 0.9868 | 0.9819 |

For primary R0, consistency-minus-V6 AUROC is -0.0147 with a paired source-family
95% bootstrap interval of [-0.0258, -0.0057]. Control-minus-V6 is also below zero
throughout its interval. Consistency-minus-V6 intervals exclude zero for Phi-4 R0
and R1, and consistency-minus-control does so for Phi-4 R0. Other intervals include
zero. Temperature-one reconstruction retains this pattern. These exploratory
intervals use 512 shared resamples per case, have no multiplicity correction and
do not turn the correlated stages into independent confirmations.

Within a source family, the generated text already scores above its own human
root in 111 of 112 primary R0 pairs for V6 and control, and 110 for consistency.
Every model achieves perfect ordering on each remaining stage/cohort. This
near-saturated comparison coexists with high human probabilities and worse
cross-family rankings. Improving pairwise ordering alone would therefore leave
much of the observed error unmeasured; a margin-based objective would require a
separate experiment.

## Where the human losses increased

The source collections differ substantially. These are descriptive slices of
the existing historical-proxy cohort, without a causal feature attribution.

| Human collection | Families | V6 log loss | Control | Consistency |
| --- | ---: | ---: | ---: | ---: |
| Historical book summaries | 50 | 0.0173 | 0.2040 | 0.4322 |
| PLOS ONE | 33 | 0.3529 | 0.3998 | 0.3050 |
| Wikinews | 34 | 0.2078 | 0.3752 | 0.6129 |

Consistency improves mean human loss on PLOS ONE while worsening it on summaries
and news. Its five largest per-human increases account for 35.73% of the sum of
positive loss increases, so they explain only part of the regression. No rows
were removed or relabeled.

I read all five scored texts in full alongside their stored provenance. The
[review record](results/origin-error-anatomy-review-v1.json) distinguishes those
observations from a fresh archive audit or independent human annotation.

| Selected source | V6 A probability | Control | Consistency | Reading observation |
| --- | ---: | ---: | ---: | --- |
| Wikinews page 193342 | 6.18% | 16.47% | 95.51% | Attributed news reporting; the excerpt starts with an unresolved reference and ends by describing an absent list |
| Book summary page 4634547 | 0.22% | 83.48% | 94.85% | Promotional synopsis, including a suspenseful closing description |
| Book summary page 23096195 | 51.47% | 95.08% | 96.69% | Promotional synopsis with visible usage and spacing defects |
| Book summary page 11423191 | 2.85% | 40.81% | 90.27% | Detailed chronological plot account, with appositives and repeated character references |
| Book summary page 4910784 | 0.70% | 56.36% | 86.69% | Plot synopsis combining participant descriptions and several threat relationships |

The four summaries match stored 2012 Wikipedia revisions; the news metadata
records a 2020 revision. These dates provide historical proxy supervision, not
documented human-only workflows. Book authors and publication dates do not
identify or date the summaries. Promotional register, formulaic narration,
source typos and missing context are useful coverage distinctions, but this
inspection does not establish that any one of them caused a prediction.

## Next experiment and validation

Keep the running larger control/consistency pair unchanged so it can answer the
capacity question. For later data work, improve Train-side human coverage across
registers and compare cross-family discrimination alongside rewrite resistance.
The five inspected Test sources must remain outside training. Any targeted remedy
will require new source families for confirmation. The observed differences do
not yet separate limited data coverage, model capacity and training-objective
effects.

Studio formatting, all-target tests, Clippy with warnings denied, and the example
build passed. Tests distinguish pooled AUROC from within-family ordering and
check temperature inversion without inventing logarithms for zero probabilities.
The [completion record](results/origin-error-anatomy-completion-v1.json) binds
the executed source, binary, results and review. Linux execution is unverified.
No NLP/ML code, frozen recipe, calibration or training split changed.
