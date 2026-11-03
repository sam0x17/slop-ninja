# Grammar workspace

This Rust workspace holds the A crates, the public A training recipe and the
spaCy-based feature crates that the detector baseline builds on.

The workspace's native grammar is not yet included in this repository. Its
crates, lexicon resources, campaign and admission records, tools, fixtures and
documents are left out for now. Links to them from other documents do not
resolve until they are added.

## Workspace

| Crate | Responsibility |
| --- | --- |
| `grammar-a-form` | Form rules for A positives: parent conventions, excerpt layout, child normaliser, screens and profile ids |
| `grammar-a-pools` | Rights-gated human prose pools for A: source adapters, overlap index and deduplicating admission |
| `grammar-a-assemble` | Training and evaluation bundle assembly for A |
| `grammar-a-positives` | Positive generation for the competitive A protocol |
| `grammar-a-augment` | Class-blind, seeded text noise for A training copies and consistency probes |
| `grammar-a-baseline` | Numeric A baselines and the examples of the public A recipe |
| `grammar-cc-human` | Common Crawl human-slice pilot sampler for A |
| `grammar-core` | Syntax representation, word and grammatical features, vector geometry, exact edit operations |
| `grammar-spacy` | Optional spaCy annotation adapter used by the feature experiments and the detector baseline |
| `grammar-markdown` | CommonMark views with mappings back to source bytes |
| `grammar-cli` | Annotation, feature extraction, profile fitting and edit enumeration |
| `grammar-corpus` | Author corpus ingestion, word counts and reference samples |
| `grammar-eval` | Author-profile and edit evaluations; `python/` holds the A trainer and the public recipe in `python/public_a/` |

The [training guide](../docs/testnet/training-guide.md) builds the public A
recipe from this workspace. The spaCy parser configuration is described in
[PARSER_CHOICES.md](PARSER_CHOICES.md).

## Local development

Run commands from `grammar/`:

```sh
cargo check
```

Use focused tests for the crates you change. Private corpora stay outside
tracked artifacts. Public dataset snapshots live in `../datasets/`.
