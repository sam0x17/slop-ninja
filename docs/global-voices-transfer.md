# Global Voices author transfer

The frozen Blog-trained model recognized the attributed author with **73.81%
top-1 accuracy** on a provisional Global Voices corpus. Its complete lexical
families scored **77.98%**; retaining the learned grammar contributions reduced
accuracy by 4.17 percentage points on this sample. We made no fitting or
calibration changes. This is a small, descriptive transfer test, not a new model
selection result.

## Collection

The [frozen collection plan](../experiments/global-voices-transfer-v1/collection-plan.json)
selected twenty accounts before article extraction or model scoring. Candidates
came from the active English contributor directory with at least forty displayed
posts. We removed twelve organizational/shared accounts identified during the
metadata review: `publica`, `balkan-diskurs`, `civic-media-observatory`,
`global-voices-cross-post`, `cee`, `global-voices-portuguese`,
`guest-contributor`, `hong-kong-free-press`, `metamorphosis`, `nepalitimes`,
`open-caucasus-media`, and `prachatai`. Among the 31 remaining candidates, we
sorted SHA-256 of `gv-author-transfer-v1:` plus the profile URL and took twenty.
This samples accounts, not verified individual or unaided writers.

The Rust collector fetched two archive pages per account and joined their
writer/translator credits to WordPress content by exact story URL. Ordinary
WordPress author IDs alone omit the custom translator and coauthor roles.
Requests used at least ten seconds between starts, matching the saved
[robots policy](https://globalvoices.org/robots.txt); there were no retries or
redirects. The final extraction reused all 62 hash-checked saved responses and
made zero network requests.

We required a single matching `Written by` credit, a matching API author, public
unprotected publication, and 150–1,500 retained words. We excluded paragraphs
with quotation markers, explicit non-English language metadata, embedded content,
captions and other nonprose structure. Whole paragraphs remain separate; we did
not delete quotation fragments and join the remaining words into new sentences.
The filter is heuristic: it can miss indirect quotations, remove a writer's own
quoted terminology, and change the distribution being measured.

Before scoring, we corrected extraction of HTML line breaks, separated image
permission notices from article-origin notices, and removed a proposed
`category-english` requirement. Cached articles with a single English-page writer
can instead carry Czech or German language tags; those tags do not establish
original composition language. Taxonomy remains in local provenance. No
translator credit observed is weaker evidence than verified original authorship.

| Qualification | Count |
| --- | ---: |
| Account–article observations | 840 |
| Distinct publisher works | 820 |
| Provisionally eligible articles | 456 |
| Selected accounts with twelve eligible articles | 14 of 20 |
| Selected articles | 168 |
| Reference / query articles | 110 / 58 |

Exclusions overlap: 321 observations failed the sole-writer credit rule, 102
contained an article-origin notice requiring review, 97 had a different API
author, 59 failed retained-length bounds, and four shared extracted text with
another distinct work. The [account counts](../experiments/global-voices-transfer-v1/account-eligibility.json)
retain the six unselected accounts and missing-response fields.

For each qualifying account, we took the earliest twelve eligible articles
**within the two scanned pages**, not the writer's earliest twelve works.
They span 2014-11-18 through 2025-07-31 and contain 73,787 retained words,
153–1,376 per article, with median 384. The first approximately two-thirds of
distinct dates supply references; later whole dates supply queries. One writer
therefore has six references and six queries; the other thirteen have eight
and four. Every reference date precedes that writer's first query date.

## Frozen evaluation

The three evaluated 3,557-coordinate models retain their Blog-trained vocabulary, numerical scales, coordinate multipliers, family weights, spline and temperature. spaCy 3.8.16 with `en_core_web_sm` 3.8.0 supplies
annotations; Rust extracts features and scores profiles. References average
articles within dates and then dates within writers. The gallery contains all
fourteen writers for every query.

| Score | Top-1 | Top-5 | Mean reciprocal rank |
| --- | ---: | ---: | ---: |
| Combined frozen model | 73.81% | 92.26% | 0.8208 |
| Complete word + word-bigram family terms | 77.98% | 93.85% | 0.8455 |
| Complete grammar family terms | 39.88% | 80.36% | 0.5693 |
| Untrained unigram Hellinger control | 72.02% | 92.26% | 0.8117 |

Metrics average queries within writers, then writers, then the three seed
summaries. Exact ties receive their expected top-k and reciprocal-rank credit.
All 168 articles parsed successfully, all fourteen reference profiles were
available, and all 58 queries were scored in every variant. The complete-family
ablations remove terms without renormalizing or refitting. Across all 2,436
query/target/seed pairs, their sum reproduced the full score within
`2.85e-14`. The unigram control uses an independent, untrained distance.

The [evaluation plan](../experiments/global-voices-transfer-v1/evaluation-plan.json)
binds the input, models, source, parser and executable.
The [result JSON](../experiments/global-voices-transfer-v1/report.json) includes
per-writer and per-seed metrics and complete coverage counts. Annotation errors
and successful parses are cached separately; an interrupted run preserves both.
An unavailable reference would disable the entire affected gallery rather than
silently remove a difficult writer.

## Implications for miner data

This screen establishes a usable local attributed-author sample. It does not
establish human-only origin labels or hundreds of qualified writers. The
publisher's default is CC BY 3.0, with attribution and item exceptions; excerpts
published in this repository carry that attribution in `NOTICE`. Source URLs, writer credits, extraction changes and
hashes appear in the [metadata-only manifest](../experiments/global-voices-transfer-v1/source-manifest.json);
article text and profiles remain ignored locally.
[Publisher attribution policy](https://globalvoices.org/about/global-voices-attribution-policy/).

Regional beats, subject vocabulary, dates and editorial practice can all aid
retrieval. We checked exact and whitespace-normalized duplicates, not semantic
near-duplicates or matched topics. Fourteen candidates also make this gallery
easier than the earlier 100-author Blog galleries, so 73.81% cannot be compared
directly with their 56.30%. The same article extraction applies to every variant,
but removing quoted paragraphs may affect each feature family differently.

Keep the current model unchanged. Before selecting a commercial starter model,
qualify more independent writers and test same-topic negatives and different-topic
works by the same writer. Learn any revised weighting on separate training
authors, then test on new writers and another register. Transformation and writing
improvement still require consented or otherwise permitted editing pairs, protected
claims and independent preferences. Retrieval scores supply none of those labels.

## Reproduction

From the repository root, collect under ignored `data/`:

```sh
cargo run --example collect_global_voices -- collect \
  --plan experiments/global-voices-transfer-v1/collection-plan.json \
  --out data/author-corpora/global-voices-v1/collection-v1
```

Add `--cache-only` to re-extract the saved responses without network access.
Live archive pages can change; reproducing the reported sample requires the
hash-bound saved responses, not merely repeating the URLs. Then run:

```sh
cargo run --release --manifest-path grammar/Cargo.toml \
  -p grammar-eval --example author_transfer -- \
  --input data/author-corpora/global-voices-v1/collection-v1/transfer.jsonl \
  --models FROZEN_MODELS/metric-seed-0.json \
  --models FROZEN_MODELS/metric-seed-17.json \
  --models FROZEN_MODELS/metric-seed-29.json \
  --out data/author-corpora/global-voices-v1/transfer-v1
```

The models remain local because this experiment uses the noncommercial Blog
Corpus research baseline. No LLM generation, Pangram call or optimizer step was
used. Formatting, tests and Clippy passed in both Rust workspaces. Two focused
collector tests cover attribution roles and paragraph extraction; scoring also
checks every observed family decomposition during the experiment.
