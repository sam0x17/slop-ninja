# Confirming the frozen spline comparison

`confirm_frontier.py` evaluates three existing architectures on a separately
frozen cohort. It does not train, fit transforms, choose epochs or select seeds.
It reuses the original development-selected checkpoints for the 45-parameter
spline, 15-parameter positive metric and 8-parameter tied metric, each with
seeds 0, 17 and 29.

Before running, freeze a protocol containing the new cohort's exact 100 source
author IDs, all 300 prior export IDs, source summary and reference-space hashes,
the nine selected checkpoints, comparison direction and bootstrap settings.
After Rust preparation, freeze an eligibility manifest containing the exact
eligible author IDs, excluded authors/posts, split counts and hashes of the
Rust implementation manifest, annotation audit and author-exclusion file.

Run from the `slop_ninja` directory:

```sh
.venv/bin/python grammar/crates/grammar-eval/python/confirm_frontier.py \
  --fit data/author-corpora/blog-authorship-2004/model-size-frontier-v1/train-v1 \
  --evaluation data/author-corpora/blog-authorship-2004/vector-evaluation-spline-confirmation-v1 \
  --protocol data/author-corpora/blog-authorship-2004/spline-confirmation-v1/protocol.json \
  --eligibility data/author-corpora/blog-authorship-2004/spline-confirmation-v1/eligibility.json \
  --out data/author-corpora/blog-authorship-2004/spline-confirmation-v1/test-v1
```

The output directory must not exist. The evaluator validates all source hashes,
model identities, frozen eligibility and query provenance before creating it.
It uses the unchanged original scoring helpers, and verifies that those helpers
still match the hashes recorded during training. CPU float64 and the pinned
PyTorch version remain required.

The primary comparison is macro-author top-1 accuracy of the spline minus the
15-parameter metric. Query metrics are averaged across the three independent
fits, then within each author, then equally across authors. The paired bootstrap
resamples those author means 2,000 times using the original fixed seed. A positive
lower 95% bound meets the predeclared superiority criterion; an interval
including zero is inconclusive. Secondary comparisons and other metrics remain
descriptive.

The output preserves the frozen protocol and eligibility manifest,
`input-validation.json`, nine complete candidate-score files, and `report.json`.
Scores are never pooled with the earlier cohort where the spline was selected
for this follow-up. Author retrieval does not establish readable editing,
meaning preservation or detector performance.
