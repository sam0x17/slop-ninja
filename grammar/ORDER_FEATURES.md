# Sentence order alongside occurrence counts

The optional extractors in `grammar_core::features::order` describe sentence
position and transitions. The default extractor and its feature identity remain
unchanged. Word occurrences, word sequences and the existing grammar families
remain available for every comparison.

The first implementation, `extract`, treats each sentence as one joint pattern:
its first three lexical POS tags, root child frame, root morphology and length
bin. It records the pattern's position in the document and paragraph, plus
transitions within paragraphs. This distinguishes some sentence permutations
that all default count families treat identically. With few reference works,
however, a detailed pattern may never match the reference distribution. Two
different points can then have the same distance from the target.

`extract_factorized` measures four descriptors separately: the first two lexical
POS tags, root child frame, root morphology and length bin. Each descriptor has
its own document-position, paragraph-position and transition distribution,
giving twelve families. Matching a common opening no longer requires matching
the entire sentence pattern. This loses some correlations between descriptors;
both representations have distinct identities for comparison.

For each representation:

- A lexical sentence contains at least one alphabetic parsed token, using the
  default extractor's definition. Length bins are 1-8, 9-16, 17-32 and 33+ lexical
  tokens. Punctuation and parser-marked spaces do not count toward length.
- Position is `floor(4 * index / sentence_count)`, with zero-based indices,
  measured once across the document and once within each paragraph. Each
  position family has one opportunity per lexical sentence.
- A paragraph break is two line-feed characters with only whitespace between
  them, including CRLF and blank lines containing spaces. Only breaks between
  parsed lexical sentences count. A paragraph boundary within one parser
  sentence cannot be represented here.
- Transitions include the first sentence's START edge, adjacent sentence pairs
  within the paragraph and the last sentence's END edge. A transition family
  has `sentences + paragraphs` opportunities. Even a single-sentence paragraph
  has two edges. Empty documents have zero opportunities, rather than observed
  zero rates.

These descriptors contain grammatical labels and length bins, with no content
words. They are lossy syntactic observations. They do not identify rhetorical
purpose, prove author identity or judge the quality of a sentence's placement.

The B CLI can audit a saved menu without changing its original measurements:

```sh
slop_ninja_transformer menu order-audit --plan /path/to/menu/plan.json \
  --representation factorized-v2 --python /path/to/spacy/python \
  --out /path/to/new-audit.json
```

Use `--representation joint-v1` for the first representation. The command fits
the order target and scales only on the menu's independent reference works.
Candidate features extend the coordinate vocabulary. It records movements
from the unchanged state and four exploratory mixtures with the original
function-word/grammar distance: order weights 0, 0.1, 0.25 and 0.5. The squared
mixture distance is `(1-w) * base_squared + w * order_squared`. These weights
are declared comparisons, not trained feature weights or quality scores.

Run extraction, fitting and evaluation on the Studio. Review the resulting
prose separately. A new representation is useful to B only if it helps select
edits that preserve meaning and intended tone, improve writing and satisfy
both detector objectives. Distinguishing previously identical vectors is the
first check, not sufficient evidence of that outcome.
