# Consolidated corpus

Slop Ninja keeps one consolidated, versioned corpus store. Version 1 lives in
ignored storage at `data/grammar-private/corpus-v1/` and is built by the Rust
[corpus store](../tools/corpus-store/README.md). It is private and additive:
it reads the existing campaign stores, published snapshots and experiment
directories, copies or references their bytes by hash, and never moves,
rewrites or deletes a source. Publication of any part needs a separate
decision and the usual redistribution checks in `datasets/`.

## What it holds

- **Texts, once each.** A text is identified by the SHA-256 of its exact UTF-8
  bytes. Every unique text is stored once as a zstd frame in an append-only
  pack, with byte, character, word (whitespace runs) and line counts, an
  English function-word language screen and, per tokenizer, a token count.
- **Exact files.** Every ingested file is archived by the digest of its
  (decoded) bytes. JSONL lines are kept as skeletons whose long string values
  refer to stored blobs, so overlapping copies of the same text cost nothing
  and any archived file, including every frozen view, can be rebuilt byte for
  byte.
- **Records.** One row per source line that carries or names a text:
  dataset, collection, domain, genre, work, family, source group, writer key,
  original document id, dates, licence, rights basis, removable cohort id,
  origin (`human_only`, `mixed`, `model_only`), measured share and AI byte
  spans, generator label and key, operation, recipe (prompt digest), parent
  id and parent text digest, admission status and recorded split. Labels come
  only from our own records; disagreements between sources stay visible.
- **Production events.** Every generation request and attempt we have
  receipts for: hosted frontier requests, attempts, withdrawals and passage
  refusals; earlier hosted captures; local llama-server calls for positives
  and for style invention, review and characterisation; planned requests; and
  stage-level generation records. Each event keeps the generator, requested
  and reported model, operation, exact prompt (by digest), parameters, raw
  receipt digest, response digest, status, times and the produced document
  when one exists. Rejected, refused, failed, interrupted and withdrawn
  attempts are kept with their status. Admission is a separate field: a
  withdrawn or rejected generation is never silently part of a Train view.
- **Detector observations.** Every Pangram observation, one row per paid
  result (`bulk:<job>:<index>` or `task:<id>`), so identical texts scored more
  than once stay separate rows, plus retained per-document predictions of our
  own detectors. Observations are comparator evidence only: they are never
  labels, targets or selection inputs, and no observation value is written to
  a record.
- **Frozen views and split membership.** Every bundle split, trainer bundle,
  split manifest, evaluation-human file, published snapshot file and stage
  dataset is registered as a view with its file digest and ordered members.
  Split membership per experiment (Train, Train-mining, held, Dev-iter,
  Dev-select, CAL, reserved and sealed Test, consumed Test, diagnostics) is a
  separate table, as are exposure and per-cohort rights rows.

## Ingestion

A plan lists sources in priority order: Pangram observations first, then
frontier inference, then local inference, then frozen views, splits,
evaluation humans and predictions, then the admitted pools, then published
snapshots and the parity and stage datasets. Each source records an input
manifest (paths, roles, digests and sizes). Ingestion is idempotent and
resumable: work is committed in batches after pack bytes are synced, an
interrupted file is redone, and a source whose inputs and adapter version are
unchanged is skipped. Every anomaly (a digest mismatch, an unparseable
journal line, a response missing from an event stream) is kept as a problem
row rather than dropped.

Sealed material is refused by path: the plan lists forbidden prefixes, and an
input that resolves under one is an error. While the Tier C Test was sealed,
its prose, positives and generation outputs were forbidden, with a single
read-only exception for the Tier C CAL Pangram store. After the Test was
marked consumed, its humans, generations, bundle, Pangram subsample and model
predictions were added as consumed evaluation evidence (split role
`test_consumed`). They are never clean held-out data.

## Rules carried over

- **Most restrictive split.** Each text gets a canonical split: the most
  restrictive exposure it ever had across every experiment, record and
  Pangram context (consumed Test, sealed Test, Test, CAL, development and
  diagnostics, held, Train). Evaluation-only is permanent and extends to
  every text of a source group that ever touched an evaluation role. Source
  groups are inherited and never re-cut.
- **Frozen views are reproduced, not changed.** Verification rebuilds every
  view file from the store and compares it with the digests its own layout
  declares (`SHA256SUMS`, bundle `BUILD.json`, and the released model's
  selection record).
- **Rights.** Per-row licences and rights bases are kept as recorded.
  Verification reports any non-commercial, research-only or academic-only
  licence, any `commercial_training: false` row and any excluded collection.
- **Source-conditioned rewrites stay distinct from writing generated from
  scratch** through the operation and ancestry fields.

## Current state

Version 1 was built on 2026-10-29 and 2026-10-30. It reads 140 sources:
about 214,000 input files and 93 GB on disk, and every source completed.

- **Texts:** 3,647,358 unique texts, 5.26 GB of exact UTF-8. Each has a token
  count under the A tokenizer. The store holds 6.46 million blobs, 54.7 GB
  decoded and 8.4 GB compressed, including the line skeletons of 158,504
  archived files.
- **Records:** 15.82 million provenance records. Overlapping copies of a text
  add records, never texts. For example, 8.79 million split-file rows name
  2.79 million texts, and 5.96 million pool rows name 3.57 million.
- **Events:** 849,989 production events:
  - 24,608 hosted frontier requests and 24,584 attempts, plus withdrawals,
    passage refusals and export accounting;
  - 96 earlier hosted captures and 20 hosted style calls;
  - 56,712 local positive-generation calls and 231,829 planned requests;
  - 27,001 local style invention, review and characterisation calls;
  - 13,371 stage-level generation records;
  - 15,186 structured-output calls of a local model spike.
- **Observations:** 253,202 detector observations in total.
  - 8,274 are Pangram observations of 8,090 texts. Every key and text digest
    in the Pangram gold index is present. The store has one extra row: a
    result page holds a successful result for an item that the index counts
    as a failure.
  - 244,928 are retained per-document predictions of our own detectors.
- **Views:** 244 frozen views. Every one rebuilds byte for byte from the store
  and matches the digests its layout declares, including the released
  model's trainer bundle.
- **Declared inputs:** 110 of the 122 files that the bundle builds record as
  inputs are stored byte for byte. The other 12 were superseded on disk by a
  later republish of the same path; the builds that used them still
  reproduce from their own rows.
- **Rights and dates:** No record carries a non-commercial, research-only or
  academic-only licence, and no excluded collection is present. All 25,195
  USPTO rows satisfy the pre-2019 filing and publication rule.

The released model's Train views hold 2,619 texts that an earlier split
version kept in an unopened diagnostic slice. A recorded decision moved them
to Train before any was opened. Verification lists that reassignment and
excludes it from the evaluation-only gate, which then counts zero.

Within one earlier supervised cohort, three texts carry records in more than
one split. Each record belongs to a different source work, so no source group
crosses splits. Verification reports them as text-level leaks in that
historical, consumed experiment.

`VERIFY.json` next to the store has the per-view and per-check detail.

## Export

`slop_ninja_corpus export --subset all` writes the complete private store as
checksummed zstd JSONL tables with a manifest. `--subset redistributable`
writes only texts that are already published in `datasets/` snapshots, with
their records, views and split membership, and no observations or events.
`--subset public-candidate-v1` writes a release candidate under a rules file:
each text and every ancestor must pass the provenance, reservation, sealed-Test,
use and rights checks listed in `tools/corpus-store/README.md`, and a denylist
gate scans every finished row. Any other text needs a separate rights and
exposure review before publication. Exact source files are rebuilt with
`rebuild`.

## Limits

- The September 2026 Pangram results recovered from the earlier workstation
  are ingested from a read-only import. Derived summaries and re-reads that
  have no task id of their own are archived but not counted as observations,
  and some earlier edit-experiment results are still missing.
- Local span, continuation and splice outputs that were never finalized are
  kept as response blobs on their events; only whole-document outputs and
  finalized records are corpus texts.
- Texts without any recorded split (mostly admitted pool rows that no split
  version selected) have no canonical split. They are not evaluation-only by
  this rule, but they still need an experiment's own admission.
- Older campaigns that have no reader yet are inventoried and queued for a
  later version.
