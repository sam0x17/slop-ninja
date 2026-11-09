# Document embeddings for author retrieval

Keep the existing full word/grammar metric as the working reference. The first
shared document encoder improved its matching untrained control on Blog data,
but transferred poorly to Global Voices. Adding a hidden layer did not help
under this recipe. These are retrospective comparisons on previously evaluated
corpora, not fresh confirmation.

| Model | Blog top-1, 600 authors | Global Voices top-1, 14 authors |
| --- | ---: | ---: |
| Existing full metric | 56.30% | 73.81% |
| Untrained document pooling control | 28.77% | 42.26% |
| Linear projection selected on development | 32.82% | 25.00% |
| Two-layer GELU projection | 27.44% | 25.00% |

The matching control and projections use only 3,557 selected coordinates and
the same document pooling and cosine score. The full metric also retains
unselected and unseen feature contributions, pools raw feature distributions
before transformation, and uses its learned family calibration. Its comparison
therefore changes more than network architecture. The follow-up below measures
the loss from removing those contributions while retaining the old scorer.

The selected linear projection gained 4.05 percentage points over its matching
control on Blog data and lost 17.26 points on Global Voices. All three linear
seeds improved the Blog control; all three fell below the Global Voices control.
This does not justify replacing the full metric or assuming that a learned
author embedding transfers between writing settings.

## Removing unselected features from the existing scorer

The existing model learns corrections for 3,557 coordinates within complete
sparse word and grammar distributions. Those 3,557 weights are not the full
dimensionality of its input: unselected and previously unseen distribution
features still contribute to distance with a default coordinate multiplier of
one. A fixed 3,557-column encoder omits those contributions.

We froze a separate diagnostic after the projection evaluation. For each
query/candidate pair, we replaced each full family distance with the sum of
squared differences over that family's selected transformed coordinates. We
retained the same raw-profile pooling, selected-coordinate multipliers, family
weights, spline calibration and temperature, without renormalizing or fitting.

| Existing scorer input | Blog top-1 | Global Voices top-1 |
| --- | ---: | ---: |
| Full feature contributions | 56.30% | 73.81% |
| Selected 3,557 coordinates only | 40.83% | 29.17% |
| Change, percentage points | -15.47 | -44.64 |

The loss is substantial under this frozen scorer, especially on the second
source. It does not assign a fraction of the projection's failure to input
coverage: the projection also changes pooling, normalization and calibration,
and those effects need not add together. The linear model's Global Voices loss
against its matching untrained control remains a separate transfer failure.

The Rust [diagnostic](../grammar/crates/grammar-eval/examples/coordinate_tail_ablation.rs)
replayed all 364,236 full candidate/seed scores with zero numerical difference
from the saved baselines. Every ranking and tie matched; all 1,264 queries from
614 authors remained in the comparison. The
[public aggregates](../experiments/author-projection-v1/tail-ablation.json)
retain each seed and gallery, removed family-distance totals, and the frozen
protocol/source hashes. All 3,792 query/seed rows and their complete rankings
remain local. This is a retrospective diagnostic, with no fitting or new parsing.

## What was new

Earlier scalar pair-score models fitted fourteen family distances. Later coordinate models learned positive diagonal corrections. Neither learned a shared mapping from individual document vectors into an author space.

This experiment fitted `3557 -> 128` and `3557 -> 256 -> 128` networks, with
455,424 and 943,744 trainable parameters respectively. The latter uses GELU
between the two linear layers; both include biases. They use the original
transformed coordinates, without the old learned multipliers or additional
standardization. We applied no dropout or batch normalization.

For a document vector `x`, compute and L2-normalize `g(x)`. Average those
embeddings within each writer's date, then average dates equally and normalize
the resulting author prototype. During fitting, remove the query's **entire
date** from its own prototype before its final normalization. Gradients still
pass through all remaining reference documents. The query's positive score is
cosine similarity to this excluded-date prototype, divided by temperature 0.1.
Other candidates use their complete reference prototypes.

This order matters: applying a nonlinear network to the old averaged raw
profile is not averaging individual document embeddings. The new Rust exporter
reads saved feature caches to provide individual reference vectors. It does
not reuse the old author-coordinate or excluded-date-coordinate matrices as
embedding prototypes.

## Data, optimization and selection

The fixed three TRAIN panels contain 3,889 posts from 300 authors, with one
hundred candidates per panel. Cross-entropy weights authors equally, then dates,
then posts within dates; each panel contributes one-third of the loss.
Development uses 514 later queries and 3,977 reference posts from 297 different
authors in three galleries. Author IDs, source groups, document IDs and exact
text hashes remain disjoint between fitting, development and evaluation exports.

The [frozen recipe](../experiments/author-projection-v1/recipe.json) specifies
seeds 0, 17 and 29, CPU float32 with eight threads, deterministic PyTorch
operations, AdamW at learning rate 0.001 and weight decay 0.01, and 200 updates
per fit. We checked development every five epochs, including zero. Checkpoint
selection uses author-macro top-1, then MRR, then the earliest epoch. Architecture
selection uses mean performance across all three seeds, with fewer parameters
as the final tie-break. We never select a best seed.

Linear development accuracy was 34.27%, versus 28.67% for GELU and 32.91% for
the untrained control. Selected epochs were 40/25/30 for linear and 30/25/45 for
GELU. Training loss continued falling after those development peaks. All six
fits completed in 37.5 seconds combined, including their development checks and
checkpoint writes, excluding input preparation and loading.

After freezing selection, we evaluated the six existing Blog test galleries
(600 authors, 8,074 reference posts, 1,206 queries) and the existing Global Voices
sample (14 authors, 110 references, 58 queries). Every requested author and
query was scored. Reported metrics average query posts within authors and then
authors equally, followed by the three seed summaries. Date balancing applies
to training loss and prototypes, not query-metric averaging. Exact score ties
receive their expected top-k and reciprocal-rank credit.

The [aggregate results](../experiments/author-projection-v1/results.json) retain
each seed, selected epoch, final training endpoint, coverage and source hashes.
Full trajectories, checkpoints, scores, corpus identifiers and features stay
under ignored `data/author-corpora/blog-authorship-2004/author-projection-v1/`.
The Blog-derived models remain local noncommercial research artifacts.

## Implementation and checks

[`document_coordinates.rs`](../grammar/crates/grammar-eval/examples/document_coordinates.rs)
exports the exact frozen columns from bound caches. Across all 5,609 Blog
TRAIN/DEV/TEST queries, every coordinate matched its earlier exported value
exactly. Source feature bytes, parser/schema identities, calendar dates and
chronological splits are checked. Global Voices uses its bound article features
and preserves their declared row order. No source parsing or external model
calls were needed.

`projection_author.py` handled PyTorch fitting and inference. It has since been
removed from the tree; its source and tests remain in Git history, for example
at commit `7f1d68db` (`grammar/crates/grammar-eval/python/`). Training refused
TEST or transfer inputs; evaluation bound the frozen selection and checkpoints
and performed zero optimizer updates. Zero or nonfinite document/prototype
norms failed explicitly. The three focused mathematical tests checked date
weighting, excluded-date invariance and gradients, reference-only evaluation
pooling, and zero-norm failure. Rust formatting, workspace tests and Clippy
passed.

With the frozen local protocol and exports, from a checkout of that commit:

```sh
.venv/bin/python grammar/crates/grammar-eval/python/projection_author.py train \
  --protocol data/author-corpora/blog-authorship-2004/author-projection-v1/protocol.json \
  --out data/author-projection-reproduction/fit
.venv/bin/python grammar/crates/grammar-eval/python/projection_author.py eval \
  --fit data/author-projection-reproduction/fit \
  --inputs EVALUATION_INPUTS_BOUND_TO_THIS_SELECTION.json \
  --out data/author-projection-reproduction/evaluation
```

The next comparison should preserve complete sparse feature coverage while
testing a learned correction. The truncation diagnostic rules out treating
3,557 learned weights as a sufficient replacement for the full input. More
layers alone did not resolve this recipe's transfer failure. Author retrieval still
does not establish meaning preservation, readability, tone control or detector
performance, which require separate evidence before an editing model is ready.
