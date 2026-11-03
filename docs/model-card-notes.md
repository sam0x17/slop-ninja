# Pangram 4 implications for this benchmark

The [model card](https://www.pangram.com/research/model-card/pangram-4) describes
overlapping 512-token inference windows, document-level sequence decoding, and
sentence-level output merging. Measure edits in full context as well as locally.

Track AI-generated and AI-assisted fractions separately and together. The
humanizer score is another output, not an input to the main sequence decoder.
Do not confuse returned segments with internal windows or class fractions with
whole-document authorship probabilities.

The card names FineWeb and FineWeb2. Public source corpora are available, while
Pangram's exact evaluation samples and filtering choices are not fully released.
See [the dataset catalog](public-datasets.md).

Our lexical and grammatical vectors are experimental measurements. They do not
reconstruct the model's hidden representation or establish which edits transfer.
