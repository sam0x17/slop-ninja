# SlopNinja author sources, 2026-10-26

Bounded acquisition on `build-host`: 26 distinct original English works from
seven authors, with complete raw Project Gutenberg text files retained under
`data/grammar-private/a-baseline-20261026/authors-v1/`. The
[preassigned source list](a-baseline-author-sources-20261026.tsv) is embedded in
the [offline Rust manifest helper](../grammar/crates/grammar-a-baseline/examples/acquire_author_sources.rs).
Acquisition is separate from excerpt selection, model fitting and detector calls;
it does not change the grammar.

| Author | Reference PG ID | Train query | Development query | Calibration query | Test query |
|---|---|---|---|---|---|
| Jane Austen | 1342 | 161 | 158 | 141 | 105 |
| Charles Dickens | 98 | 1400 | 730 | 766 | 1023 |
| Mark Twain | 74 | 76 | 86 | 1837 | 3177 |
| H. G. Wells | 35 | 36 | 5230 | 159 | 1013 |
| Arthur Conan Doyle, withheld | — | — | 244, 2097 | — | — |
| Robert Louis Stevenson, withheld | — | — | — | 120, 43 | — |
| Mary Shelley, withheld | — | — | — | — | 84, 18247 |

One original work forms one source group, `author_work_v1:<author_key>:pg<pg_id>`; all later
editions, excerpts, windows and derivatives of that work must inherit its
group and partition. Reference and query works are distinct. Withheld authors
have two query works each and no enrollment/reference samples. Historical
transcriptions are a human-author proxy, not documented contemporary workflows.
Raw PG files retain front/back matter; attribution-qualified excerpt selection
is a separate transform.

The helper takes the acquisition directory as its sole argument and writes
`manifest.json` and `manifest.json.sha256` there. It performs no downloads.
Its `sources` array provides `pg_id`, `author_key`, `author`, `title`, `split`,
`role`, `purpose`, `source_group`, absolute `text_path`, `text_sha256`, byte
length, requested/effective URLs, dates, and `rights`. Reference sources have
`split: "train"`, `role: "train_reference"`, `preassigned_partition: "train_reference"`,
and `purpose: "reference"`; all others are queries. Query splits are `train`,
`development`, `calibration`, or `test`. It checks the requested four-by-five
and three-by-two author/partition structure, exact catalog author/title,
English text format, US public-domain status on both retained catalog formats,
successful HTTP/TLS receipts, byte lengths, and unique file hashes. Source
bytes are streamed only for SHA-256 and byte counting, including Test.

Per-work rights evidence is the retained primary page
`https://www.gutenberg.org/ebooks/<pg_id>` and its `.rdf` catalog. Each identifies
the work as **Public domain in the USA.** Both come from the same publisher.
The manifest sets `commercial_training` and `external_evaluation` to `true` for
the underlying US public-domain work, with that scope and conditions stated in
each record. Gutenberg's
[permission policy](https://www.gutenberg.org/policy/permission.html) permits
commercial use and derivatives; its [license](https://www.gutenberg.org/policy/license.html)
governs copies retaining PG material and describes removal of PG references
under section 1.C. Full embedded notices remain in the
unaltered raw files; this acquisition does not inspect in-work notices or prose.

Exact primary pages, RDF, rights-policy pages, response headers, and curl
receipts are retained in `evidence/` and beside `downloads/pg<id>.txt`.
The manifest binds their SHA-256 hashes. PG release and file-update dates are
explicitly ebook metadata. MARC 260 publication statements are retained
verbatim when available; first-publication and composition dates are left
null because they were not separately verified. Automated summaries supply
no dates, labels, or sample-selection decisions.

Downloads completed sequentially with an 8 MiB per-text bound. The Rust helper
caps aggregate source bytes at 64 MiB. The excerpt builder takes two reference
excerpts per reference work and two query excerpts per query work. Builds and
execution used task-owned memory-capped jobs on build-host, without CI compute.

The acquisition produced all 26 manifest records (18,057,446 raw
text bytes). The separate extractor produced 40 non-Test records, with no
failures and `test_text_opened: false`: eight reference excerpts and 32 query
excerpts. Twelve Test excerpts from six works were opened only after feature,
model and calibration selection froze. Their results and derived-text provenance
are described in the [supervised baseline report](../grammar/crates/grammar-a-baseline/SUPERVISED_BASELINE.md).
The source-manifest SHA-256 is
`61e6a210da4ef4667213a4f9996f0de23fe14663bf2089ffdfd25c12a4a4de4d`.
Catalog extents differ from downloaded sizes for PG 161 (+10 bytes), PG 730
(+3 bytes), and PG 18247 (-4 bytes). Exact HTTP transfer lengths match the
retained files; both catalog and actual sizes remain recorded, without
silently substituting a different edition or normalizing text.
