# Saved A models fit Train but still fail to generalize

All six runs in the [frozen diagnostic](TRAIN_FIT_DIAGNOSTIC_V1.md) completed:
V6, base control and base consistency each scored all 3,833 Train records in
both original and collapsed-whitespace form. Every run retained all 471 source
families. Quality results were opened only after all six succeeded.

The human/model distinction is already well fitted on these training examples.
Three-class provenance remains harder, especially the distinction between a
fully generated passage and a model edit of a historical human source.

| Saved model / view | Binary accuracy | Binary NLL | Equal-family/origin binary NLL | Three-class accuracy | Three-class NLL |
| --- | ---: | ---: | ---: | ---: | ---: |
| V6 raw | 99.50% | 0.02609 | 0.02681 | 83.02% | 0.39754 |
| V6 whitespace | 98.89% | 0.03535 | 0.05368 | 82.44% | 0.41032 |
| Control raw | 99.50% | 0.02392 | 0.03887 | 82.29% | 0.39532 |
| Control whitespace | 99.16% | 0.02992 | 0.05182 | 82.62% | 0.39988 |
| Consistency raw | 99.65% | 0.01606 | 0.02653 | 86.98% | 0.33630 |
| Consistency whitespace | 99.39% | 0.02259 | 0.04449 | 87.40% | 0.33585 |

These are original calibrated scores. Binary evaluation excludes the 1,227 mixed
rows and combines model-only and mixed probabilities when scoring the other
2,606 rows. Three-class evaluation includes every row. Equal-family/origin
metrics average within each family/origin cell before averaging cells; every
family has all three origins here. No temperature or threshold was refitted.
Human labels retain the existing historical-proxy evidence limits.

Temperature-one binary NLL ranges from 0.00882 to 0.03032. Its three-class NLL
ranges from 0.31647 to 0.40169. Removing calibration therefore does not eliminate
the contrast between strong binary Train fit and weaker provenance separation.
The [summary](results/train-fit-summary-v1.json) retains both temperatures,
confusions, class contributions, collection slices and original-cutoff counts.
An independent Studio recalculation reproduced the reported NLL, accuracy,
Brier scores, family/origin averages and cutoff counts within 1e-9.

Raw human NLL is 0.02740 for V6, 0.06152 for control and 0.04258 for consistency.
In the whitespace view it rises to 0.08191, 0.08535 and 0.07845 respectively.
Consistency improves several generated/mixed distinctions while retaining
formatting-sensitive human errors. At the original strict and loose cutoffs,
human Train false-positive counts are respectively 1/8 and 3/29 for V6 raw and
whitespace, 1/4 and 1/5 for control, and 0/2 and 1/6 for consistency. These are
observed Train counts, not achieved deployment false-positive guarantees.

The [error selection](results/train-fit-error-selection-v1.json) nominates the
highest wrong-class score across the six runs for each human collection and
four model/mixed confusion directions. The
[manual review](results/train-fit-error-review-v1.json) read all eight complete
texts and the four generated texts' source passages. The
[review binding check](results/train-fit-error-review-check-v1.json) verifies
the selected IDs, text hashes and exact source passages embedded in the saved
generation prompts. This is deliberately selected error analysis, not
representative sampling or independent human gold.

One narrative summary differs between views only by a leading ASCII space.
Removing that byte raises V6's combined model/mixed probability from 14.25% to
97.18%. Every other byte is identical. Both newer fits also score that source
highly. A news report moves in the other direction when paragraph separators
collapse. A research abstract has identical bytes in both views and still
exceeds one half combined probability under all three detectors. Formatting
sensitivity is real, but does not explain all observed errors.

The reviewed human sources already include promotional institutional prose,
rhetorical narration and conventional abstract language. A Qwen draft can
closely resemble its technical source while V6 misses model involvement. An
OLMo draft and a heavily rewritten OLMo copyedit expose opposite model/mixed
confusions; both still score highly for model involvement. The requested
copyedit also changes facts and motives. Process provenance, realized editing
intensity and semantic fidelity need separate annotations; scores cannot decide
those labels retrospectively.

Together with the [earlier public comparison](ORIGIN_CONSISTENCY_RESULTS_V1.md),
strong binary Train fit supports a generalization problem. It does not identify
the exact shortcut or prove that a larger model cannot help. The saved checkpoints
are selected epochs, and this diagnostic does not reproduce online dropout,
sample weights or consistency penalties. Three-class fit is also incomplete.
Finish the unchanged larger-capacity comparison before choosing the next fit
recipe. Broader source coverage, formatting invariance and the relationship
between process labels and the training objective remain concrete hypotheses.

No new training data, private text, paid provider call, threshold, model promotion
or whitepaper accuracy milestone follows from this diagnostic. All inference
and audit processing ran on the Studio. These are exposed Train measurements,
not fresh confirmation or evidence that A now tracks Pangram.
