# Standard corpora and reusable Pangram annotations

Decision, 2026-09-11: build a versioned corpus with independent origin evidence
and separately stored detector observations. Start with a local benchmark, then
publish the portions for which both text and annotation reuse rights are clear.
A Pangram prediction must never overwrite an origin label or establish that an
unlabeled web page was written by a human.

The [first public text snapshot](../../datasets/detector-origin-v1/README.md)
now contains 432 admitted records with their source attribution, exact prompts,
generator provenance and frozen Train/Development assignments. Individual
Pangram annotations may be published as comparator evidence, never as origin
labels; they stay in ignored storage until a release includes them.
The [hosted research snapshot](../../datasets/detector-hosted-v1/README.md)
adds 96 GPT/Claude texts with complete prompts and 24 supporting source roots.
Training admission for these outputs comes from the
[hosted admission manifest](../../docs/a-hosted-output-admission.json), not from publication.

## Dataset priorities

| Source | Contribution | Admission work |
| --- | --- | --- |
| [RAID](https://github.com/liamdugan/raid), initially book summaries | Paired sources, multiple generators, decoding settings and adversarial variants | Reconstruct `source_id` and `adv_source_id` families. Join human summaries to [CMU Book Summaries](https://www.cs.cmu.edu/~dbamman/booksummaries.html), whose Wikipedia-derived text is CC BY-SA. Preserve attribution and the applicable version of that license. Other RAID domains need a per-source licence record. |
| [HC3](https://github.com/Hello-SimpleAI/chatgpt-comparison-detection#dataset-copyright), initially `wiki_csai` | Wikipedia answers paired with ChatGPT answers | Its copyright table identifies this subset as CC BY-SA. Recover article/revision attribution and group all answers to the same source together. Other subsets need a per-source licence record. |
| [FineWeb](https://huggingface.co/datasets/HuggingFaceFW/fineweb/blob/main/README.md) | Broad web registers for false-positive stress testing | ODC-By covers the database; its card also points to Common Crawl terms. Recover page-level rights and composition evidence. Crawl time alone does not prove human authorship. Not admitted as a whole collection. |
| Existing rights-admitted PLOS/Wikinews pairs | Immediate end-to-end annotation rehearsal, including model-edited historical text | Already tracked locally. Preserve the weak historical labels and existing splits. These narrow registers cannot substitute for the broader benchmark. |

RAID and HC3 are established detection datasets, while FineWeb supplies broad
web text. Pangram's [current model card](https://www.pangram.com/research/model-card/pangram-4)
reports FineWeb evaluations; downloading a similarly named collection would not
reproduce its exact sample or labeling policy. Public benchmark performance also
cannot establish that a provider never trained on the benchmark.

The initial import work should stage pinned RAID/HC3 subsets under ignored
`data/`, produce source joins and license evidence, and report exclusions before
admission. The detector loader now admits verified Wikipedia excerpts with the
explicit [ShareAlike policy](SHARE_ALIKE_POLICY.md), attribution and change notices.
Those obligations pass through generation, feature extraction and model release.
A book's author is not the author of its Wikipedia plot summary.

## Corpus contract

Each document retains its upstream dataset revision and row ID, exact UTF-8
text hash, source family, derivation parents, origin label, evidence strength,
rights evidence and split. Keep upstream binary labels binary when production
records cannot support our three-class distinction. Attack metadata does not by
itself establish human involvement in a mixed-origin workflow.

Store Pangram observations in a sidecar keyed by exact text hash, requested
selector, returned version, timestamp and repeat index. Keep complete raw
responses, task/bulk IDs, request hashes and errors. Preserve document fractions
and window results separately. `fraction_ai + fraction_ai_assisted` measures
flagged content; it is not our document-origin probability. Missing and failed
requests remain visible. No score-based exclusions or replacement texts.

Freeze source-family splits before annotations. Use training observations for
error mining or an explicitly declared auxiliary teacher experiment. Development
observations support selection; confirmation annotations stay sealed until the
contender and analysis are frozen. Reuse each observation across all detector
comparisons. If the provider changes, append a new observation set. Never mix
versions silently or call agreement with Pangram accuracy against true origin.

For a standard public release, ship a corpus card, source manifest, reconstruction
code, documented labels and uncertainty, fixed splits, checksums, attribution and
the permitted annotation fields. Separately maintain fresh private confirmation
families. Customer inference text and private correspondence remain excluded.

## API and cost

Pangram documents bulk jobs with a 1,000-unit limit: each text consumes one unit
per started 100-word block for Pangram 4. Results expire 48 hours after terminal
completion, so archive them promptly. Save the initial bulk receipt before
polling, reconcile every accepted or failed item, and never automatically repeat
an uncertain submission. Pin the selector explicitly and record the returned
version. [Bulk API documentation](https://docs.pangram.com/api-reference/bulk-api).

On 2026-09-11 the posted rate is $0.05 per Pangram 4 unit, with 20% off for bulk
jobs. At exactly 300 words each, 1,000 documents cost an estimated $120 for one
bulk pass; 10,000 cost $1,200. Three complete repeats triple those figures.
Per-document rounding matters: 301 words require four units. Actual provider
word counting and account billing must be checked during a small rehearsal.
[Pricing](https://www.pangram.com/pricing).

Under the project's publication policy, row-level Pangram results may be
published as comparator evidence. A published result never becomes an origin
label, and each text's own source licence still decides whether the text itself
is redistributed. Hosted OpenAI and Anthropic outputs, including those produced
through the command-line clients, may also be published, and so may local
open-weight model outputs, each with its model's licence or terms notice.
Publication remains subject to the providers' terms and to third-party rights.

## Implemented preparation step

`annotation_plan` reads an already frozen, admitted origin corpus. It checks
external-evaluation permission, deduplicates exact inputs while retaining every
source label, and writes Pangram 4 bulk payloads plus a costed manifest. Each
planned repeat has a distinct request ID. It makes no network calls and reads no
API key. Its archived corpus and request files contain text and belong in ignored
`data/`. The Rust `pangram_bulk` command now submits against a cumulative local
budget, archives receipts, resumes collection and reconciles every result.
It refuses to repeat an existing or uncertain submission. `compare_pangram`
compares the annotated Development corpus with the frozen local models;
`bulk_repeatability` compares the three complete observations for every input.

```sh
cargo run --release --manifest-path baseline/detector/Cargo.toml --bin annotation_plan -- \
  --input data/baseline-detector/style-pilot-v2/studio-results/data/primary-shards/development.jsonl \
  --output-dir data/baseline-detector/pangram-corpus-v1/development-plan
```

The prepared 57-row batch contains 15,250 whitespace-counted words. Its exact
request payload hash is
`cd192f9652c9bcf630350a05f6844da7000cc9e536331d16eb7ef18581b89401`.
A read-only catalog request confirmed `pangram-4` access for the available key.

This prepares the exact development corpus as an API rehearsal. It is
already used for model selection and cannot provide fresh confirmation. The
separate word/grammar probe remains free of provider calls. With subsequent user
authorization, all 57 documents received three provider observations; the
estimated charge is recorded in the private ledger. See
[results and limits](PANGRAM_CORPUS_RESULTS.md). Raw provider annotations
remain local.

## Local standard-dataset staging

The Rust `stage_hc3` command fetched only `wiki_csai.jsonl` and its dataset card
at revision `4d0ff18143b5a7e1b1e79beb540c04549d1e59d3`. The local capture has 842
questions, 842 human-labeled answers and 842 ChatGPT-labeled answers. Of the 1,684
answers, 62 fall below Pangram's stated 50-word scope. The JSONL hash is
`4f78105c72e7edd3b619b3b44abac1e6f8bdb42211f22e43191d4dae889e1968`.
See the [capture manifest](manifests/hc3-wiki-staging-v1.json).

The rows contain only `question`, `human_answers` and `chatgpt_answers`: there
are no upstream row IDs or article/revision URLs. This makes reconstruction of
Wikipedia source attribution the next concrete admission task. Local hashes can
identify rows but cannot supply missing authorship or license evidence. The raw
subset remains in ignored `data/baseline-detector/standard-corpora/hc3-wiki-v1/`;
none of it has entered training or been sent to Pangram.

```sh
cargo run --release --manifest-path baseline/detector/Cargo.toml --bin stage_hc3 -- \
  --output-dir data/baseline-detector/standard-corpora/hc3-wiki-v1
```

The staging command requires a new directory, bounds downloads, archives exact
bytes and records hashes. It does not execute the upstream dataset loader.

## Historical-lineage review

`resolve_hc3` used article titles embedded in the questions to review a fixed
100-row sample, ordered by SHA256 of the question. It retrieved each article's
last revision at or before 2022-11-01 and required the whole normalized answer
to occur both in the rendered lead and in historical wikitext after conservative
markup removal. Templates are not expanded for the literal match, preventing a
current template from supplying the only evidence of historical wording.

Of 100 rows, 99 resolved to usable historical source captures. Forty-nine
answers matched both checks, fifty did not, and one lacked revision text.
Forty matched answers had none of the initial rights-notice search terms; 35 of
those contained at least 50 words. The keyword screen is provisional and can
produce false positives or miss a notice. It is not license certification.

The [review summary](results/hc3-lineage-v1.json) binds the exact input and local
result hashes. Captures and individual decisions remain in
`data/baseline-detector/standard-corpora/hc3-lineage-v1/`. This supplies 49
historical source joins for further admission work; it exports no training
records. Wikimedia accepts article links as one route to contributor attribution
and requires preservation of additional imported-text notices where present.
[Wikimedia reuse terms](https://foundation.wikimedia.org/wiki/Policy:Terms_of_Use#7._Licensing_of_Content).

## Admission and corpus expansion

The complete 842-question historical review is finished. `admit_hc3` rechecks
capture hashes, revision identity and full text containment before applying the
declared rights, quotation and length rules. It admitted 261 human excerpts from
243 Wikipedia source families. The frozen partitions contain 190 Train, 28
Development, 26 Calibration and 17 Test records. Multiple HC3 questions can map to
one article; keep those rows in one family and select one representative per
family for the first generation run.

The importer excluded 405 historical mismatches, 112 length failures, 16 long
quotations, 43 source notices and five unresolved sources. It admitted no upstream
ChatGPT answers. These are historical proxies, with the source and label limits
described in the policy. The corpus hash is
`178b2c9ab55a74107fb6792b72a5d69e369a9bd103b17fa3343caeaac989c23d`.
The [admission summary](results/hc3-admitted-v1.json) binds every review batch,
exclusion corpus, decision log and the importer executable. Raw records are in
ignored `data/baseline-detector/standard-corpora/hc3-admitted-v1/`.

The separate `stage_books` command captured CMU Book Summaries: 16,559 unique
Wikipedia page IDs and 43,461,583 extracted bytes, SHA256
`94516c64d1b4ac6b8b397ef106ee598b43d0e5364ffd8305edc945dce04f4305`.
There are 9,368 summaries in the current 80-500 lexical-word range and five empty
summaries. Its download returned a 2013 Last-Modified timestamp. The collection
page links to CC BY-SA 3.0 **US**, which must be preserved alongside upstream
Wikipedia rights. Source-specific historical checks remain before admission;
the book's publication date does not date its summary. See the
[capture manifest](manifests/cmu-books-staging-v1.json) and
[CMU collection page](https://www.cs.cmu.edu/~dbamman/booksummaries.html).

The [narrative source review](CMU_SOURCE_REVIEW.md) now specifies a fixed
100-summary historical match sample and preservation of both the CMU collection
license and upstream Wikipedia notices. `resolve_books` performs that review;
`admit_books` rechecks the captures and exports qualifying historical proxies.
The [completed admission](CMU_SOURCE_RESULTS.md) retained 289 of 400 summaries,
including all 75 admitted in the initial 100-source sample. All initial partitions
were preserved. The [v5 protocol](ENCODER_NARRATIVE_V5.md) defines the next
generation campaign and reserves Phi-4 for Test-only transfer evaluation.
