# Author retrieval evaluation

This Rust runner evaluates the existing dependency and word-occurrence features
on an `unslop-corpus export-authors` directory. It uses the local spaCy adapter and
does not call an LLM or an AI detector.

```sh
cargo run --release -p grammar-eval -- \
  ../data/author-corpora/blog-authorship-2004/authors-v1 \
  --out ../data/author-corpora/blog-authorship-2004/evaluation-v1 \
  --python ../.venv/bin/python \
  --batch-size 32
```

Choose the Python interpreter with the installed spaCy model. `--cache` can point
to an existing annotation cache. Otherwise the cache lives under the output
directory. Reusing an output directory requires identical input bindings and
protocol. Cached annotations must match the current parser identity and exact
source; cached features must match fresh extraction from those annotations.

Before annotation, the runner validates every input hash, word count, author and
split count, composition date, and chronological date-group assignment. Exact
and whitespace-normalized duplicates anywhere in the selected export cause an
error. Each parsing or feature-availability failure is recorded and excluded
from every representation. An author must retain train, dev and test examples,
with at least two training date groups.

The evaluation freezes one shared core space using all training date groups.
Query feature names extend only its categorical vocabulary. Query frequencies
and labels do not fit any author target, numerical scale, IDF or weight. This is
a vocabulary-only transductive basis, and the report declares it explicitly.

Author targets average each post's raw distributions and measurements within a
date, then average dates equally. Distribution probabilities are square-rooted
after that averaging. Numerical measurements use the shared training scales.
Sparse distances give the same result as projecting these targets and queries
into the dense core space; the tests check that equivalence.

The fixed representations are:

- The original combined weights: half of the weight for word and word-bigram
  occurrence, half for the other twelve families.
- Word and word-bigram occurrence, with their retained weights renormalized.
- The twelve dependency and grammar families, with their retained weights
  renormalized. Numerical scales remain frozen.
- A content-lemma TF-IDF baseline using NOUN, PROPN, VERB, ADJ and ADV tokens,
  training-only document frequencies, normalized post vectors, and normalized
  date-group author means. This is a topic proxy, not verified topic labels.

Dev and test are scored separately, in that order, without selecting weights
between them. Reports include macro-author and query-micro top-1, top-5 and mean
reciprocal rank, plus 2,000 fixed-seed author-bootstrap confidence intervals.
Exact ties receive expected metric values over tied positions. A diagnostic
compares the true author against the nearest other author under the content
proxy, retaining all equally close impostor IDs. This diagnostic does not make
the corpus a cross-topic benchmark.

The output preserves the frozen space, sparse author targets and training
bindings, ablation weights, content IDF and targets, annotation audit, complete
source provenance and all candidate distances/rank ranges for every held-out
post. Exact annotation and raw-feature caches permit an offline numerical audit.
All corpus text and derived profiles belong in ignored local `data/`.

## Stability and learned weights

`unslop-stability` measures training-only matched grammar concordance.
`unslop-tune` selects among a fixed small weight grid on the original development
queries. `unslop-learn-weights` trains a positive one-layer metric with Adam and
selects its checkpoint on the same development authors. Use `--help` for their
input paths. The [model description](../../../docs/README.md) explains
the loss, exclusions and comparisons.

For fresh authors, pass `--reference-space ORIGINAL_SPACE.json` and
`--selection SELECTION.json` to `unslop-evaluate`. The evaluator rejects overlap
with the selection authors and verifies the original source-space identity.
New author training posts fit their reference means. The old numerical scales
remain fixed while new categorical names extend the basis. Results include the
selected weights, a lexical ablation, and paired differences against the word
and combined baselines. Each completed run writes artifact hashes and the
executed binary hash to `implementation.json`.

`unslop-export-coordinates` supplies named coordinates for the
[individual-coordinate weighting experiment](../../../docs/README.md).
Run from the `slop_ninja/` project root with the original evaluation, export,
annotation cache and frozen space:

```sh
cargo build --release --manifest-path grammar/Cargo.toml \
  -p grammar-eval --bin unslop-export-coordinates
grammar/target/release/unslop-export-coordinates EVALUATION_DIR \
  --export EXPORT_DIR --cache CACHE_DIR --space ORIGINAL_SPACE.json \
  --split train --protocol PROTOCOL.json --out data/coordinate-reproduction/train
grammar/target/release/unslop-export-coordinates VALIDATION_EVALUATION_DIR \
  --export VALIDATION_EXPORT_DIR --cache CACHE_DIR --space ORIGINAL_SPACE.json \
  --split dev --catalog data/coordinate-reproduction/train/catalog.json \
  --protocol PROTOCOL.json --out data/coordinate-reproduction/validation
```

Each output directory must be new. Training fits the catalog using only the
exact original space's training sources. Validation and test exports require
that frozen catalog and reject overlap with its fitting authors. Use
`--split test` for a held-out test export. No parser or model is invoked: the exporter
validates cached annotations, re-extracts features, and checks source hashes and
evaluation bindings.

Categorical coordinates require nonzero values in at least 10 fitting authors
and 30 fitting posts. Within each family, rank by author support, then post
support, then feature name; retain up to 512 words, 512 word bigrams, and 96
coordinates from each other categorical family. Numerical coordinates must
vary across fitting posts. The catalog preserves every support and selection
decision, including original vocabulary-basis names with no fitting support.
Those names cannot become selected coordinates.

`manifest.json` records ordered authors and families, array shapes and hashes,
catalog and query hashes, parser/space identities, input bindings, and executed
source/binary hashes. `catalog.json` names the selected coordinates;
`queries.json` records query IDs, source dates/groups, true-author indices and
author/date/post-balanced loss weights. Arrays use little-endian float64 values
in row-major order, with `Q` queries, `A` authors, `K` selected coordinates and
`F` complete feature families:

| Array | Shape | Content |
|---|---|---|
| `query-coordinates.f64` | Q × K | Transformed query values |
| `author-coordinates.f64` | A × K | Full training author targets |
| `own-heldout-coordinates.f64` | Q × K | Training only: true-author target after excluding the query's entire date |
| `family-distances.f64` | Q × A × F | Complete original unweighted family distances, with the training true-author target replaced as above |

Categorical coordinates are `sqrt(p / 2)` after averaging raw probabilities
within and across dates. Numerical coordinates are
`value / (original_scale * sqrt(original_family_axis_count))`; centering cancels
in pair differences. Neither transformation includes family weights. Subtract
selected coordinate contributions from each complete family distance to retain
the unselected and unseen remainder. No vocabulary renormalization or dense
query × author × coordinate array is needed.

`unslop-export-coordinate-frontier` exports all supported coordinates once and
records nested subsets for the
[coordinate-capacity experiment](../../../docs/README.md). Build
this new binary separately to preserve the previous exporter:

```sh
cargo build --release --manifest-path grammar/Cargo.toml \
  -p grammar-eval --bin unslop-export-coordinate-frontier
grammar/target/release/unslop-export-coordinate-frontier ORIGINAL_EVALUATION_DIR \
  --export ORIGINAL_EXPORT_DIR --cache CACHE_DIR --space ORIGINAL_SPACE.json \
  --split train --original-catalog ORIGINAL_COORDINATE_EXPORT/catalog.json \
  --reference-export ORIGINAL_COORDINATE_EXPORT --protocol PROTOCOL.json \
  --out data/coordinate-frontier-reproduction/train
grammar/target/release/unslop-export-coordinate-frontier FRESH_EVALUATION_DIR \
  --export FRESH_EXPORT_DIR --cache CACHE_DIR --space ORIGINAL_SPACE.json \
  --split dev --catalog data/coordinate-frontier-reproduction/train/catalog.json \
  --protocol PROTOCOL.json --out data/coordinate-frontier-reproduction/validation
```

Run from the project root and use fresh output directories. Change `--split dev`
to `--split test` for a prepared test gallery. Training recomputes support from
the exact original training posts and requires equality with the original
catalog. The exporter verifies cached annotations and source bindings through
the same checks as the earlier exporter.

The frontier catalog schema is `slopninja-coordinate-frontier-catalog-v1`.
Categorical support remains at least 10 authors and 30 posts; numerical axes
must vary in training. Every eligible categorical coordinate has a one-based
`support_rank` within its family, ordered by author support descending, post
support descending, then feature name ascending. Coordinates remain in
family/feature order in the binary arrays. Full support records include rejected
axes and their reasons.

`catalog.subsets` defines word, grammar and combined selections at cap
multipliers `0.5`, `1`, `2`, `4`, `8`, and without a cap. Each entry contains
sorted `max_column_indices`, per-family caps, an index-array hash and a
`canonical_name` for deduplicating identical selections. Names follow
`all_m0p5`, `word_m1`, `grammar_m8`, and `all_supported`. Word selections cover
`word` and `word_bigram`; grammar selections cover the other families. Numerical
scales and full-family denominators remain fixed at every size.

The manifest schema is `slopninja-coordinate-frontier-export-v1`. Array names,
shapes and encoding match the preceding exporter, using the full catalog's
column count. Select columns by subset indices and retain the complete family
distances for contributions outside that subset. `reference-reproduction.json`
records mandatory training checks: the original `all_m1` coordinate arrays,
complete family distances and query metadata must reproduce the previous export
byte for byte. Source snapshots and corpus artifacts belong in ignored `data/`.
