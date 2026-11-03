# Selecting slopninja scorers on different authors

`author_selection.py` separates the authors used to train a scoring model from
the authors used to select its checkpoint and architecture. It trains on the
original 100-author distance tensor, selects on development posts from a new
100-author source cohort, and tests the frozen selections in three further
100-author galleries. All four new cohorts exclude every account in the four
earlier exports and each other.

The experiment compares two positive scoring functions for each feature subset:

| Inputs | Weighted sum | Shared monotone spline |
|---|---:|---:|
| Words and word pairs | 3 parameters | 33 parameters |
| Twelve grammar families | 13 parameters | 43 parameters |
| Words and grammar | 15 parameters | 45 parameters |

Each model learns only the weights for its included families and one
temperature. Spline models additionally learn 30 positive interval slopes.
There are no unused trainable weights for excluded families. Initialization
renormalizes the original family priors within each subset. Each spline uses
30 fixed nested dyadic quantile knots fitted to its subset's original training
distances. Calibration therefore differs across subsets as well as the inputs.

Each cell receives the same three seeds and fixed 200-update Adam schedule.
Training retains the original author/date/post-balanced loss and whole-query-
date exclusion from the true author's profile. Numerical distance scales remain
frozen. The model never uses original development queries or validation-cohort
test queries. New validation authors supply their own training reference
profiles, and their development posts select checkpoints.

Within each seed, checkpoint selection uses validation macro-author top-1, then
MRR, then earliest epoch including zero. Within each feature subset, architecture
selection uses the three selected checkpoints' mean validation scores, then
fewer parameters and model name. No seed is selected. Every fit must complete;
any failed fit stops the study with its failure record preserved.

The primary comparison is the validation-selected combined pipeline minus the
validation-selected lexical pipeline. It compares complete fitted and selected
procedures, including their different training calibrations. It is not a pure
causal test of adding grammar while holding every other component constant.
Fixed-architecture differences and all six cells are secondary comparisons.

Run from `slop_ninja` after freezing the protocol and validation eligibility:

```sh
.venv/bin/python grammar/crates/grammar-eval/python/author_selection.py train \
  --original-fit data/author-corpora/blog-authorship-2004/neural-weight-transfer-v1/fit-v1 \
  --validation-evaluation data/author-corpora/blog-authorship-2004/vector-evaluation-disjoint-validation-v1 \
  --validation-eligibility data/author-corpora/blog-authorship-2004/author-disjoint-selection-v1/validation-eligibility.json \
  --protocol data/author-corpora/blog-authorship-2004/author-disjoint-selection-v1/protocol.json \
  --out data/author-corpora/blog-authorship-2004/author-disjoint-selection-v1/train-v1
```

Only after `selection.json` exists should the test eligibility manifest bind its
SHA256. Then run `evaluate` with `--fit`, `--protocol`, `--eligibility`, `--out`,
and three `--evaluation` arguments in the frozen protocol's gallery order.
Both commands require a new output directory.

Every seed retains all 201 checkpoints with model and optimizer state, loss,
validation metrics and exact input/source hashes. The test report retains each
gallery's metrics, every candidate score, and the pooled result. Pooling weights
authors equally, not galleries equally when eligibility reduces their sizes.
The paired bootstrap resamples authors within each gallery, preserving its
eligible size, and averages all sampled authors. It reports a 2,000-replicate
interval using the original fixed random seed. A positive lower 95% bound is the
predeclared superiority criterion.

The test set comprises three separate author-retrieval tasks. A query competes
against its own gallery's profiles, not the union of all three galleries.
Content-matched impostor metrics are diagnostic only; the report exposes
unavailable content proxies and tied impostors. No training, checkpoint
selection or architecture selection occurs during testing.
