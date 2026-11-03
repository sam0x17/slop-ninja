# Local Blog Authorship experiments

The user selected the [Blog Authorship Corpus](https://u.cs.biu.ac.il/~koppel/BlogCorpus.htm)
for local noncommercial experiments. Keep its archive, texts, database, author
manifests and derived profiles in ignored `data/author-corpora/blog-authorship-2004/`.
The source permits noncommercial research; this workflow does not authorize a
public corpus release or commercial subnet use.

Use the original author files. The alternative Hugging Face loader emits lines
and omits author IDs, which loses the units needed to compare writing styles.
The Rust `grammar-corpus` crate preserves those units and uses
`grammar_core::features::words` for word occurrences. Its database stores raw
counts and denominators separately from vector geometry.

Run from `grammar/`, after downloading the original ZIP to the local data path:

```sh
cargo build --release -p grammar-corpus
target/release/unslop-corpus import-blog \
  ../data/author-corpora/blog-authorship-2004/raw/blogs.zip \
  --out ../data/author-corpora/blog-authorship-2004/import-v1
target/release/unslop-corpus export-authors \
  ../data/author-corpora/blog-authorship-2004/import-v1/corpus.sqlite \
  --out ../data/author-corpora/blog-authorship-2004/authors-v1 \
  --authors 100 --min-posts 10 --min-post-words 150 \
  --max-post-words 1200 --min-total-words 3000 --max-posts-per-author 20
```

Both commands require a new output directory. The importer retains the archive
hash, entry hashes, post byte spans and text hashes, along with rejection reasons.
It reads archive members without extracting their paths to disk. Reference
writing from the blogging era is acceptable without a hard 1999 cutoff.
Preserve each supplied composition date; the chronological export still needs
valid dates to keep date groups separate across its splits. Older saved imports
retain the age policy recorded when they were created.

`corpus_words(word, occurrences)` holds counts over retained posts;
`author_words(author_id, word, occurrences)` holds the same counts per author.
`authors.total_words` supplies each author's denominator. Sum those totals for
the corpus denominator. `posts.status` explains exclusions, while its flags
identify placeholder and encoding issues kept in the full counts. The stricter
author export excludes those flagged posts by default.

```sql
SELECT word, occurrences FROM corpus_words ORDER BY occurrences DESC LIMIT 20;
SELECT SUM(total_words) AS corpus_total_words FROM authors;
SELECT status, COUNT(*) AS posts FROM posts GROUP BY status;
```

The [upstream loader](https://huggingface.co/datasets/barilan/blog_authorship_corpus/raw/main/blog_authorship_corpus.py)
uses Latin-1. Importing uses an explicit reversible Latin-1 byte mapping and keeps
raw-byte hashes separate from decoded UTF-8 text hashes. This does not prove the
intended encoding of every historical post. Exclude posts with C1 control
characters from the grammar pilot instead of silently correcting probable
encoding artifacts.

Preserve punctuation and exact decoded post text. Whitespace-normalized hashes
are for duplicate detection; they do not replace the text used for syntax.
Exclude whole posts containing the source's `urlLink` placeholder from the pilot
so removal does not join unrelated phrases. Quoted or copied passages and
non-English writing can remain in this corpus. Exports record that their quality
and authorship have not been independently verified; inspect samples before
treating them as style targets.

Sampling uses stable seeded hashes, with length and author-volume thresholds.
Keep an author's samples in a separate manifest. Split chronologically by date,
keeping posts from one date together and reserving nonempty development and test
sets. Revisions or duplicate text must not cross splits. Count eligibility after
the sample cap, not just in the full author archive.

Use only training posts to fit a target profile. Keep all compared points in the
same frozen vector space, and keep whole authors aside when evaluating changes
to the grammar representation. Differences between authors can reflect topic,
genre, age or copied text as well as style. An author corpus supplies reference
writing; it does not validate a detector score or establish meaning preservation.

The database and author exports require no NLP model. Annotation and evaluation
run the installed local spaCy parser. No LLM or detector service is part of this
workflow.

## Evaluate author profiles

After export, run from `grammar/`:

```sh
cargo run --release -p grammar-eval -- \
  ../data/author-corpora/blog-authorship-2004/authors-v1 \
  --out ../data/author-corpora/blog-authorship-2004/vector-evaluation-v1 \
  --cache ../data/author-corpora/blog-authorship-2004/vector-feature-cache-v1 \
  --python ../.venv/bin/python --batch-size 32
```

The runner validates all exported hashes, counts, dates and duplicate exclusions
before parsing. It caches exact annotations and features, fits one shared space
from training date groups, and compares word-only, grammar-only, combined and
content-lemma TF-IDF profiles. It writes separate development and test metrics,
author-bootstrap intervals and all candidate scores. The content baseline and
nearest competing author diagnostic help assess topic overlap; they do not
replace independently labeled topics. See the [evaluation protocol and geometry](../grammar/crates/grammar-eval/README.md).

For a fresh cohort, exclude the original author set explicitly:

```sh
target/release/unslop-corpus export-authors \
  ../data/author-corpora/blog-authorship-2004/import-v1/corpus.sqlite \
  --out ../data/author-corpora/blog-authorship-2004/authors-replication-v1 \
  --seed blog-author-replication-v1 \
  --exclude-authors-from ../data/author-corpora/blog-authorship-2004/authors-v1/summary.json
```

The exporter validates the prior archive/database identity and author counts,
then records the excluded IDs and prior summary hash. Keep the original cohort
for training and development when selecting feature weights, and reserve
independent writers for transfer evaluation.
