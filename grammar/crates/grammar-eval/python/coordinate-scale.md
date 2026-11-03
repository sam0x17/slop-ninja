# Reproduce the training-author scale comparison

This study compares the fixed 3,557-coordinate `all_m2` model fitted on 100
versus 300 authors. It preserves the original catalog, numerical scales, family
coefficients, spline, temperature and coordinate bounds. It adds the earliest
two previously evaluated cohorts with 100 eligible authors: replication and
spline confirmation. Their original TRAIN posts supply the additional queries;
their former DEV and TEST posts remain outside fitting.

Each of the three fitting panels contains 100 authors, giving every query one
positive and 99 negatives. The positive profile excludes the query's entire
date. Within each panel, cross-entropy weights authors equally, then dates,
then posts. The trainer accumulates each panel's gradient with weight one third,
adds the coordinate penalty once, and performs one Adam update. It uses 200
updates, learning rate 0.03, betas (0.9, 0.999), epsilon 1e-8, CPU float64 and eight
threads. Coordinate multipliers start at 1 and remain between 0.5 and 2; the
penalty is lambda/14 times the sum of within-family mean squared log weights.

The four lambdas 0.1, 1, 10, 100 and three inherited baseline states require twelve
new treatment fits. The twelve existing 100-author control fits are reused.
The runner verifies their 2,412 checkpoint hashes and rederives checkpoint and
lambda selection from their saved validation histories. It also reproduces the
selected control's metrics on the exact same 297-author, 514-query DEV set.
There are no control optimizer updates.

From the repository root, with the pinned ML environment:

```sh
corpora=data/author-corpora/blog-authorship-2004
study="$corpora/training-author-scale-v1"
frontier="$corpora/coordinate-frontier-v1"
.venv/bin/python grammar/crates/grammar-eval/python/coordinate_scale.py train \
  --control-fit "$frontier/train-v1" \
  --baseline-fit "$corpora/author-disjoint-selection-v1/train-v1" \
  --incumbent-fit "$corpora/coordinate-weighting-v1/train-v1" \
  --training-export "$frontier/coordinates-train-v1" \
  --training-export "$study/coordinates-training-replication-v1" \
  --training-export "$study/coordinates-training-confirmation-v1" \
  --validation-export "$frontier/coordinates-validation-1-v1" \
  --validation-export "$frontier/coordinates-validation-2-v1" \
  --validation-export "$frontier/coordinates-validation-3-v1" \
  --protocol "$study/protocol.json" --inputs "$study/training-inputs.json" \
  --out "$study/train-v1"
```

Every treatment epoch from 0 through 200 is eligible for checkpoint selection by
validation macro-author top-1, then MRR, then earliest epoch. Select one shared
lambda by the three selected seed results' mean top-1, then MRR, then stronger
regularization. All twelve treatment fits must finish; a failed fit stops the
run and preserves its artifacts. Each epoch and checkpoint remains recorded.

After model selection, freeze test eligibility and inputs against its SHA256,
then evaluate six fixed source cohorts in protocol order:

```sh
.venv/bin/python grammar/crates/grammar-eval/python/coordinate_scale.py evaluate \
  --fit "$study/train-v1" --control-fit "$frontier/train-v1" \
  --baseline-fit "$corpora/author-disjoint-selection-v1/train-v1" \
  --incumbent-fit "$corpora/coordinate-weighting-v1/train-v1" \
  --export "$study/coordinates-test-1-v1" \
  --export "$study/coordinates-test-2-v1" \
  --export "$study/coordinates-test-3-v1" \
  --export "$study/coordinates-test-4-v1" \
  --export "$study/coordinates-test-5-v1" \
  --export "$study/coordinates-test-6-v1" \
  --protocol "$study/protocol.json" --inputs "$study/test-inputs.json" \
  --out "$study/test-v1"
```

The primary comparison is the selected 300-author treatment minus the frozen
selected 100-author control. Average each author's metrics across the three
inherited seed pairs before the 2,000-replicate paired author bootstrap, stratified
by the six galleries. A positive lower 95% bound is the superiority criterion.
Uniform test eligibility exclusions apply to every variant without replacement.
The incumbent, family baseline and conditional coordinate resets are secondary;
their intervals are descriptive and unadjusted. Resetting every learned
coordinate must exactly restore the family baseline.

This is a comparison at fixed optimizer-update count. It also adds training
posts and topics, uses more computation, and remains conditional on these panels
and inherited states. The validation set is explicitly reused for development;
new test outcomes never select models, epochs or seeds.

```sh
.venv/bin/python -m unittest discover -s grammar/crates/grammar-eval/python -p 'test_*.py'
.venv/bin/python -m pip check
```
