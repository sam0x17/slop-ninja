# Diagnose the base candidates' human-error regression

The completed origin-consistency experiment improves one known private failure
while worsening human log loss and the seven fixed corpus comparisons. Before
changing a recipe, inspect whether the public regression reflects confidence,
ranking, or both. This is an exploratory analysis of already opened results.

Use the same V6, control and consistency reports and all seven declared cases.
Keep model-only and human-only rows for binary discrimination; exclude mixed
rows from that objective. Measure ordinary pairwise AUROC and the fraction of
source families whose model output scores above their own human root. Use the
same source-family bootstrap draws across models for AUROC differences.
Repeated stages reuse families and do not supply independent replications.

Also reconstruct temperature-one probabilities from saved calibrated
three-class probabilities: normalize `exp(T * log(p_i))`, using each artifact's
original temperature. This recovers relative unscaled logits, subject to saved
floating-point precision. Reject zero probabilities rather than silently
inventing their logarithms. Compare these diagnostic losses and rankings with
the deployed calibrated scores; fit no replacement temperature or threshold.
Three-class temperature scaling can change the ranking of the collapsed
model-plus-mixed probability, so do not assume AUROC is invariant here.

For the 117 human roots, retain each source collection, evidence status and
per-model observations. Inspect the five largest consistency-minus-V6 human
log-loss increases in full, with provenance and source context where available.
Report how concentrated the regression is. These adaptively selected examples
are error analysis, not new confirmation or training admission. Preserve the
historical evidence caveat for human proxy labels.

All computation and text inspection run on the Studio. No model/provider call,
private manuscript access, threshold change, reference selection or training-data
admission is part of this diagnostic. Broader human coverage and fresh evaluation
remain necessary whichever explanation fits these observations.
