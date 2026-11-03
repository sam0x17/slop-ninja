# Reproduce the coordinate-weight study

Run these commands from the repository root with the Python environment in
[requirements-ml.txt](requirements-ml.lock) and [training-lock.json](training-lock.json). Corpus files, coordinate exports,
checkpoints and results remain in ignored `data/` directories.

The commands below record the completed run. Existing output directories are
never overwritten. For another fit, choose a fresh output directory and bind
its new selection digest in a separate test-input manifest. To replay the
existing frozen model, keep its fit and input paths and choose a fresh
evaluation output directory.

The Rust exporter supplies selected query and author coordinates, complete
unweighted family distances, and the positive candidate's held-date target for
training. Python learns one log multiplier for each supported coordinate.
The three inherited family/spline/temperature states remain fixed. At zero log
multiplier the scorer exactly reproduces the inherited baseline, including every
unselected coordinate's contribution.

```sh
study=data/author-corpora/blog-authorship-2004/coordinate-weighting-v1
baseline=data/author-corpora/blog-authorship-2004/author-disjoint-selection-v1/train-v1
.venv/bin/python grammar/crates/grammar-eval/python/train_coordinates.py train \
  --baseline-fit "$baseline" \
  --training-export "$study/coordinates-train-v1" \
  --validation-export "$study/coordinates-validation-v1" \
  --protocol "$study/protocol.json" \
  --inputs "$study/training-inputs.json" \
  --out "$study/train-v1"
```

The frozen protocol specifies 200 full-batch Adam updates at learning rate 0.03,
betas (0.9, 0.999), epsilon 1e-8, deterministic CPU float64 and eight threads.
Each log multiplier starts at zero and stays between minus and plus ln(2).
The penalty is the mean squared log multiplier within each nonempty selected
family, averaged equally across those families, multiplied by lambda.
The complete grid uses lambdas 0.01, 0.1, 1 and 10 against each inherited seed
0, 17 and 29. A failed fit stops the study and prevents selection.

For every fit, select among epochs 0 through 200 using validation-author macro
top-1 accuracy, then MRR, then the earliest epoch. Select one shared lambda by
the mean of the three selected seed results, with MRR and then stronger
regularization as tie breakers. This reuses the preceding study's author-disjoint
validation DEV split. Validation TEST and all new test outcomes are unused.

Training saves all 2,412 checkpoints and epoch records, source hashes, the
selected catalog, zero-weight identity checks, and `selection.json`. The
input manifest binds the frozen protocol, source cohort identities, eligibility,
and export hashes. The scorer checks the inherited source snapshots and states
before fitting or evaluating.

After `selection.json` exists, freeze the test input manifest with its SHA256.
Supply the three test exports in protocol order:

```sh
.venv/bin/python grammar/crates/grammar-eval/python/train_coordinates.py evaluate \
  --fit "$study/train-v1" \
  --baseline-fit "$baseline" \
  --export "$study/coordinates-test-1-v1" \
  --export "$study/coordinates-test-2-v1" \
  --export "$study/coordinates-test-3-v1" \
  --protocol "$study/protocol.json" \
  --inputs "$study/test-inputs.json" \
  --out "$study/test-v1"
```

Evaluation averages each author's metrics across the three inherited seed pairs.
It retains all per-seed scores and candidate logits. The primary comparison is
coordinate weighting minus the matched frozen baseline. Its paired 95% interval
uses 2,000 author bootstrap samples, stratified by gallery, with the predeclared
xorshift seed and percentile indices 49 and 1949. Each eligible author has equal
weight, including when gallery sizes differ. A positive lower bound is the
predeclared confirmation criterion. There is no training or selection in this
command.

Run the numerical and provenance tests with:

```sh
.venv/bin/python -m unittest discover -s grammar/crates/grammar-eval/python -p 'test_*.py'
.venv/bin/python -m pip check
```
