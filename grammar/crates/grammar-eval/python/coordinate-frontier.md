# Reproduce the coordinate frontier

This experiment varies which coordinates can change and how many supported
coordinates receive learned weights. Every scorer retains all fourteen original
feature families. “Words adjustable” permits corrections to word and bigram
coordinates; grammar still contributes through the frozen baseline.

Rust exports a maximal catalog derived from the original 100 training authors:
7,341 categorical coordinates with support from at least ten authors and thirty
posts, plus twelve original numerical coordinates. The numerical scales and
complete family distances stay fixed. Unselected coordinates retain weight one.

| Cap multiplier | Words adjustable | Grammar adjustable | Both adjustable |
| --- | ---: | ---: | ---: |
| 0.5 | 512 | 460 | 972 |
| 1 | 1,024 | 840 | 1,864 |
| 2 | 2,048 | 1,509 | 3,557 |
| 4 | 2,189 | 2,449 | 4,638 |
| 8 | 2,189 | 3,740 | 5,929 |
| All supported | 2,189 | 5,164 | 7,353 |

The three largest lexical catalogs are identical and share one fit, leaving
sixteen configurations. For each, fit lambdas 0.1, 1, 10 and 100 against the three
inherited baseline states. Only the selected coordinate log weights train.
Each starts at zero, stays within ±ln(2), and uses 200 full-batch Adam updates
at learning rate 0.03, betas (0.9, 0.999), epsilon 1e-8, CPU float64 and eight
threads. The penalty is lambda divided by fourteen, multiplied by the sum of
the within-family mean squared log weights. Inactive families contribute zero,
so changing the learning mask preserves a coordinate's shrinkage coefficient.

Three new validation galleries select checkpoints and configurations using DEV
queries only. Average metrics within each author, then equally across eligible
authors. Select each fit's checkpoint by top-1 accuracy, then MRR, then earliest
epoch (including zero). Select a shared lambda across the three inherited seeds
by mean selected validation top-1, then MRR, then stronger regularization.
Select configurations by those means, then fewer trainable coordinates, then
canonical ID. No seed selection or logit ensemble occurs.

Run from the repository root with the pinned ML environment:

```sh
study=data/author-corpora/blog-authorship-2004/coordinate-frontier-v1
baseline=data/author-corpora/blog-authorship-2004/author-disjoint-selection-v1/train-v1
incumbent=data/author-corpora/blog-authorship-2004/coordinate-weighting-v1/train-v1
.venv/bin/python grammar/crates/grammar-eval/python/coordinate_frontier.py train \
  --baseline-fit "$baseline" --incumbent-fit "$incumbent" \
  --training-export "$study/coordinates-train-v1" \
  --validation-export "$study/coordinates-validation-1-v1" \
  --validation-export "$study/coordinates-validation-2-v1" \
  --validation-export "$study/coordinates-validation-3-v1" \
  --protocol "$study/protocol.json" --inputs "$study/training-inputs.json" \
  --out "$study/train-v1"
```

The runner requires all 192 fits to finish before selection. It retains all
38,592 checkpoints and epoch records, source hashes, catalogs and aliases,
validation metrics, training losses, and overlap checks against the previous
study's identical combined trajectories. A failed fit stops the grid and leaves
its failure record in place.

After selection, freeze the test eligibility and input manifests with the
selection hash, then evaluate the three separate test galleries:

```sh
.venv/bin/python grammar/crates/grammar-eval/python/coordinate_frontier.py evaluate \
  --fit "$study/train-v1" --baseline-fit "$baseline" --incumbent-fit "$incumbent" \
  --export "$study/coordinates-test-1-v1" \
  --export "$study/coordinates-test-2-v1" \
  --export "$study/coordinates-test-3-v1" \
  --protocol "$study/protocol.json" --inputs "$study/test-inputs.json" \
  --out "$study/test-v1"
```

The single primary comparison is the frozen validation winner minus the prior
1,864-coordinate incumbent. The paired bootstrap averages metrics across the
three inherited seed pairs before resampling authors within each gallery.
It uses 2,000 replicates and the frozen xorshift seed and percentile indices.
A positive lower 95% bound establishes superiority under this protocol.

The report also includes every configuration's selected-lambda model, the frozen
family baseline, and conditional resets of the selected combined model's learned
lexical or grammatical corrections. These comparisons are descriptive and their
intervals are unadjusted. Resetting every correction must exactly restore the
family baseline. The evaluator does not train or select models.

```sh
.venv/bin/python -m unittest discover -s grammar/crates/grammar-eval/python -p 'test_*.py'
.venv/bin/python -m pip check
```
