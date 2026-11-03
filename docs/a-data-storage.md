# Reusable A data and CFG evidence

Retain each admitted document's exact text, source-family and production history,
split, rights and exposure records, CFG parse evidence, and raw CFG vector. A new
training run should reuse this preprocessing when its complete input contract
matches. Growing the corpus must add independent source works, not merely many
descendants of a few works. All descendants stay in their source family's split.
Dataset growth does not reopen a reserved Test or change an experiment in flight.

Private corpora, fitted profiles and live provider records remain in ignored
`data/grammar-private/`. Redistributable publication snapshots use `datasets/`
only after rights and provenance checks. The original recovered V6 release and
its datasets remain immutable. Existing paid Pangram observations remain
permanent evidence; compression does not authorize replacement or deletion.

The completed encoder experiment's 4,157 Train queries from 357 works are
reconstructible from the [published training index](../datasets/a-retained-train-20261026-v1/training-reconstruction.json).
Its compressed membership preserves original fit order, text hashes, source
groups and all target/role masks, referencing four separately checksummed
snapshots. Public identity-index meanings are retained without private profile
vectors or weights. These are the original fitting inputs, not a new training
population; source licenses and historical/exposure caveats remain in each row.

Private R2 storage is authorized for large CFG archives, fitted models and
retained experiment evidence. Keep the bucket private, with its managed public
URL disabled and no custom domains. The [Rust artifact store](../tools/artifact-store/README.md)
creates consistent SQLite snapshots and bounded, content-addressed bundles,
resumes uploads, verifies every remote object by reading it back, and restores
exact file bytes against pinned manifests. It retains all local sources. A
prepared local snapshot is not an independent backup until remote upload and
restoration are verified. R2 is now enabled and the private bucket exists;
upload and per-object readback verification are in progress. The independent
remote download/restore has not yet completed. Current preparation and recovery
state live under the Stage3 task's `r2-storage-v1/` directory.

The current backup set contains six independently restorable snapshots:
5,243,615,664 compressed bytes for the historical-plus-Test CFG archive,
1,354,283,656 for the original/Linux V6 parents and six fitted exports/checkpoints,
72,383,367 for original datasets/provenance/paid observations, and 155,343,998 for
published data, code and numerical qualification. Two acquisition supplements
add 37,986,350 and 20,646,443 bytes, preserving the completed Pangram comparison,
new local and hosted rewrite batches, exact spans, raw receipts and quality limits.
All 101,122 files restored byte-for-byte locally. The total is 6.88 GB compressed
in 188 chunks; the restored CFG index covers 12,492 documents and 31,140 file
bindings. These local round trips do not establish remote backup completion.

The successor CFG snapshot followed both completed backfills and preserved its
predecessor. Upload records the exact selected manifests; an independent remote
restore receipt is written only after every snapshot succeeds. Later style-corpus
parse additions require another snapshot, rather than changing these frozen
manifests or implying that a running writer is already backed up.

Published dataset payloads are stored as lossless zstd throughout published Git
history. Their original decoded paths and checksums remain the compatibility
contract. Use the [Rust dataset storage utility](../tools/dataset-storage/README.md)
to verify archives or restore a snapshot before running existing readers. This
storage migration does not change frozen grammar identities, split assignments,
or exposure records. Old Git IDs in experiment receipts remain original evidence;
the history migration supplies an alias map separately.

## What is retained

The Rust `cfg_vector_cache` example stores one SQLite database per frozen parsing
profile. Exact-text SHA256 identifies reusable computation; dataset IDs and
provenance are separate aliases. Its profile binds the native parser, document
policy, resolver, feature schema and exporter implementation. Different historical
profiles cannot be pooled. Raw sparse expected counts, token opportunities,
fallback use, incomplete search and projection availability remain explicit.
The cache fits neither vocabulary nor normalization. Those belong to each
model's Train-only frozen feature contract; IDs and hashes are never coordinates.

Completed document exports can be imported without parsing again. Imports verify
row and receipt digests and exact-text/accounting bindings. They retain forest
paths and digests but do not reread large forests. The separate archive step
reads and verifies those forests. A cache hit does not make an incomplete parse
complete, and a stored execution failure remains a failure.

The Rust `cfg_parse_archive` example makes a self-contained compressed copy of
each cached document's text, features, receipt, aliases and retained forest.
Each document remains independently addressable. Its versioned `SNCFGP1` format
stores window-local node indices, symbol/string dictionaries, variable-length
integer spans and child references, and all alternatives. Zstandard level 3
compresses the stream. Shared subtrees are not expanded. Original node order and
root indices survive; dictionary positions are storage addresses, not stable
model features or cross-version grammar IDs. Original lexical/production IDs and
structural signatures remain in the dictionaries.

A single depth-first tree traversal would discard ambiguity and duplicate shared
subtrees. It remains a possible view of a selected derivation, not the canonical
archive. Every newly archived forest is decoded and checked against the original
JSONL SHA256 before the archive index commits. Metadata bytes, including floating
point spellings, are preserved exactly. Compressed files have their own checksums.
An interrupted conversion leaves the original intact and can resume per document.

Training JSONL, manifests, generation/span provenance and other immutable text
artifacts can also be archived byte-for-byte with Zstandard. Small index databases
remain queryable; active tensors and checkpoints need not be compressed if it
would slow fitting or save little. Keep original paths while live jobs, manifests
or recovery commands depend on them. No command here deletes source files.

## Reuse after a grammar change

The archive indexes observed lexical and production IDs to find some affected
documents. This is a scheduling hint, **not proof that other documents are
unchanged**. Added rules or readings can introduce new alternatives without
touching an old dependency. Tokenization, fallback policy, resolver or feature
meaning changes also require a separately identified successor profile.

Current general-document exports retain all alternatives within recognized
islands, but not the entire chart frontier. Projection failures can retain only
lexical recovery. They cannot directly use the campaign's conservative production
certificates. The [existing incremental cache](../grammar/PARSE_PROVENANCE_CACHE.md#conservative-production-certificates)
requires complete frontier and exposure commitments; unsupported or unresolved
cases must be replayed. Do not claim incremental grammar migration is implemented
by this archive. A later bridge must capture the required evidence and validate
certified reuse against fresh parsing. Preserve predecessor parses and vectors;
never relabel them under a new grammar hash.

When only model normalization or vocabulary changes, the raw vectors can be
reused under the same feature meanings. Retained forests also permit future
feature reprojection without grammar search, once that reprojection is implemented
and separately versioned. Stable semantic feature identities must not depend on
forest position, dictionary position or a Merkle path through a particular tree.

## Commands and recovery

Build the parser cache and standalone archive tool in bounded jobs on build-host,
with a separate build directory for each workspace:

```sh
cargo build --release --locked --manifest-path grammar/Cargo.toml \
  -p grammar-native --example cfg_vector_cache
cargo build --release --locked --manifest-path tools/cfg-storage/Cargo.toml
```

The archive utility has its own dependency lock because the parser identity
includes the entire grammar workspace lockfile. Adding a storage-only dependency
there would change the frozen parsing profile despite unchanged parser code.
The standalone utility preserves the original indexed format and decoded-byte
checks; it does not relabel caches under a different profile.

Use captured binaries for a running experiment. Paths below are placeholders;
cache and archive roots must be under ignored `data/grammar-private/`.

```sh
cfg_vector_cache init REPO CACHE FROZEN_EXPORT_CONTRACT
cfg_vector_cache import CACHE COMPLETE_DOCUMENT_EXPORT
cfg_vector_cache ensure REPO CACHE RECORDS NON_TEST_MANIFEST
cfg_vector_cache export CACHE RECORDS NON_TEST_MANIFEST NEW_VECTORS.jsonl
cfg_vector_cache status CACHE
cfg_vector_cache audit CACHE RECORDED_INPUT NEW_MISSING.jsonl NEW_REPORT.json
cfg_parse_archive sync REPO CACHE ARCHIVE
cfg_parse_archive file REPO ARCHIVE IMMUTABLE_DATA_FILE
cfg_parse_archive status ARCHIVE
cfg_parse_archive restore ARCHIVE TEXT_SHA256 NEW_WINDOWS.jsonl
cfg_parse_archive restore-file ARCHIVE FILE_SHA256 NEW_FILE
```

`init` requires a fresh cache. Resume interrupted preprocessing with `ensure`,
not `init`. The parser loads only on a miss; each completed document commits
independently. `export` requires complete population coverage and a new output.
`ensure`/`export` accept bound non-Test manifests. A final Test uses its separately
sealed evaluation path; import its completed exports afterward. Archived prior
Tests remain consumed evaluation evidence, not fitting data.

`ensure-recorded REPO CACHE RECORDS HISTORICAL_CORPUS_MANIFEST` is the separate
resumable path for already exposed archives, including their old Test splits.
The `slop_ninja_recorded_corpus_preprocessing_v1` manifest binds exact records
and provenance-manifest hashes, declares the historical split,
`preprocessing_only: true`, `exposure: historical_consumed_population` and
`new_reserved_test_included: false`. Original records and splits stay unchanged.
These assertions require a documented previously exposed population; they cannot
authorize opening a new holdout or moving old Test data into Train. Auditing
reports cache presence only, without admitting data into an experiment.

`sync` reads a snapshot of committed cache keys without holding up the parser.
Run it again after a concurrent backfill finishes to capture new documents and
aliases. It verifies existing compressed parses before reuse and writes a receipt
per completed snapshot. Only one archive writer and one cache writer are allowed;
they can run concurrently because they write separate databases. Retain the
SQLite WAL with its database while a writer is live. An archive on the same disk
is durable local storage, not an independent backup.

The current bounded A experiment uses
`data/grammar-private/a-encoder-stage3-20261026/cfg-vectors-v1/`, with runners and
recovery state in that task's `HANDOFF.md`. Its initial import contains 5,368
distinct vectors and 5,423 dataset bindings from twelve completed exports.
The new 1,628-document non-Test population has completed backfill. This is the
admitted A population, not a claim that every historical corpus archive has
been vectorized. Keep historical-profile and coverage exclusions explicit.

The recovered V6 snapshot contains another 5,463 recorded documents. Its audit
found 597 already cached and 4,866 absent (3,371 Train, 480 Development, 490
Calibration and 515 historical Test). The bounded backfill is complete under the
original frozen parser profile. No archived row is added to the
current frozen fitting or selection populations.

The completed Stage3 Test adds 636 queries and four independent references.
`prepare_consumed_test_cfg_cache STAGE NEW_OUTPUT` requires its completed sealed
six-run evaluation, verifies source/prediction bindings, and preserves every
source field while adding a checked `split: test` alias for the historical cache
reader. It cannot admit these consumed works into fitting or open a fresh holdout.
Its bounded cache/archive job waits for the historical V6 backfill to finish,
then uses the same captured parser and archive binaries. The preparation and
receipts live under `final-test-v1/cfg-cache-prepare-job-v1/` and
`final-test-v1/cfg-cache-job-v1/`. All 640 consumed-Test records now have cached
vectors and archived parses. The completed cache contains 12,492 distinct texts
and 13,154 dataset bindings under profile
`b3d91eed678bf5b918e307084e7211092bdb56c8a31a8bb26b87c0f2d1742acf`.
These are storage-coverage counts, not complete grammatical analyses or fitted
training counts. New style-acquisition batches use the same profile and retain
their separate provenance and training-admission status.

A completed reuse check exported all 929 rows of the previous non-Test cohort
without loading the parser: 18.13 seconds, 54,692 KiB peak process RSS on one CPU.
Its one native-complete and 928 explicitly incomplete/fallback statuses were
preserved. This measures cache export, not first-time inference or parsing.

One real 14,632,882-byte forest restored byte-for-byte after conversion. It used
471,514 bytes with ordinary JSON+Zstandard and 279,850 bytes with indexed
forests+Zstandard, demonstrating a benefit beyond compression alone in this case.
The completed historical-plus-Test archive contains 12,492 documents and 31,140
file bindings. It holds 134,968,869,616 original forest bytes in 3,277,257,573 indexed compressed
bytes. These are archive sizes, not disk reclaimed: dependent originals remain.
