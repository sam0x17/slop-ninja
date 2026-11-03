# Explicit word and grammar inputs for A

This preparation uses the unchanged V6 Train, Development and Calibration
archives for the [feature comparison](CAPACITY_AND_GRADIENT_TRANSFER.md).
It fits coordinate vocabularies and scales, with no classifier fitting or
detector scoring. It does not replace the active encoder experiments.

Merge the three full archives with the existing Rust `style_pilot merge`
command. Retain all 4,903 records and their existing source-family assignments;
do not take only the latest revisions. Test, hosted captures and private B text
are excluded. The merge validates provenance and checks conflicting identities.
Parsing uses the existing detector bridge with `en_core_web_sm`, whole-source
input and exact source hashes. Keep this identity separate from paragraph
profiles and the transformer parser. Their vectors are not interchangeable.

Extract one combined word/grammar cache with `slop_ninja_detector featurize`.
The existing input limits reject unsupported text without truncation. A parser
failure stops preparation; it never produces an all-zero replacement row.
Keep the complete cache and logs on the Studio. Although the cache omits raw
source text, its word and n-gram keys still reveal text information. It is not
an anonymized data release.

Use the Rust `export-vectors` command to fit three spaces:

| Representation | Total coordinate cap | Word/bigram cap | Grammar cap |
| --- | ---: | ---: | ---: |
| Word | 8,192 | 8,192 | 0 |
| Grammar | 8,192 | 0 | 8,192 |
| Combined | 16,384 | 8,192 | 8,192 |

Use minimum Train document frequency two and the existing RMS floor of 0.01.
Availability coordinates are retained. Unused capacity does not transfer
between the two combined budgets. Verify that combined coordinates, document
frequencies and scales match the corresponding standalone spaces. Neither
Development nor Calibration may contribute coordinates or scale statistics.

```sh
slop_ninja_detector featurize --input records.jsonl --output features.jsonl \
  --mode combined --python /path/to/studio/python --batch-size 128
slop_ninja_detector export-vectors --features features.jsonl \
  --output-dir combined-vectors --mode combined \
  --max-coordinates 16384 --word-coordinate-cap 8192
```

The export contains the frozen feature space, sparse partition rows, attribution
obligations and a manifest binding their hashes and executable/source identity.
Every row retains its exact text hash, parser-derived feature hash and source
group. The neural model consumes coordinate values; labels, identifiers, hashes
and source metadata remain outside its input. Family availability coordinates
distinguish measured zeros from unavailable measurements. Unknown-coordinate
counts and unavailable-family names remain explicit diagnostics.

These are full source archives. Use the original R2 Development and Calibration
views when fitting and comparing neural models, with the same source-origin
weighting and revision pairing as their declared controls. The exporter neither
selects an evaluation view nor chooses a neural architecture, optimizer or
checkpoint. Record those choices before fitting.

Inspect complete source/annotation examples before interpreting grammar effects.
The small parser has known POS and attachment errors, and successful extraction
does not establish correct syntax. Occurrence-to-span maps for edit suggestions
remain separate work. A better feature classifier would still need held-out
edit-direction and Pangram transfer checks before its gradients guide B.

## Completed preparation

The [export check](results/explicit-features-export-check-v1.json) verified all
4,903 rows and output-file hashes. The standalone word and grammar spaces each
contain 8,192 coordinates; their union exactly matches the combined space's
16,384 coordinates, including document frequencies and scales. No classifier
has been fitted from these exports.

The [first annotation inspection](results/explicit-features-manual-review-v1.json)
read three complete states from one Train family and found specific dependency
and tokenization errors. The [remaining nine examples](results/explicit-features-manual-review-v2.json)
have now also been read, with all sentence roots and selected dependencies
inspected. These are partial syntax reviews, not exhaustive gold annotations.
Identical cached and replayed features establish reproducibility only.

The [neural comparison protocol](FEATURE_NN_V1.md) defines the next fitting
experiment and its controlled comparisons.

During preparation, direct JSON serialization to files caused many small writes.
A subsequent buffered writer preserved byte-identical output for all 4,903
word-mode records. Extraction and writing took 79.03 seconds with the old binary
and 9.33 seconds with the buffered binary in this [qualification run](results/buffered-writer-qualification-v1.json).
Other Studio workloads remained active, so this is not an isolated throughput
benchmark. The completed grammar/vector preparation retained its original binary.
