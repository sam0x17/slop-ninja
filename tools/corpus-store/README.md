# corpus-store

`slop_ninja_corpus` builds and checks the consolidated SlopNinja corpus: one
SQLite database plus append-only zstd packs of content-addressed blobs. It
reads sources only, never writes to them, and makes no network calls. The
private store lives in ignored storage; see
[the consolidated corpus](../../docs/a-consolidated-corpus.md).

```text
slop_ninja_corpus --store DIR init
slop_ninja_corpus --store DIR manifest --plan PLAN.json [--source ID...] --out INVENTORY.json [--stat-only]
slop_ninja_corpus --store DIR ingest --plan PLAN.json [--source ID...] [--force] [--threads N]
slop_ninja_corpus --store DIR link
slop_ninja_corpus --store DIR tokens --tokenizer tokenizer.json [--threads N]
slop_ninja_corpus --store DIR rebuild --file-sha256 SHA --out NEW_FILE
slop_ninja_corpus --store DIR verify --out VERIFY.json [--expectations EXP.json] [--rebuild-views]
slop_ninja_corpus --store DIR status
slop_ninja_corpus --store DIR export --out NEW_DIR [--subset all|redistributable]
slop_ninja_corpus --store DIR export --out NEW_DIR --subset public-candidate-v1 --rules RULES.json [--repo-root DIR]
```

- **Plan** (`slop_ninja_corpus_plan_v1`): sources with an adapter, a priority,
  a root and ordered inputs (relative paths or `*`/`**` globs with a role and
  format), plus forbidden path prefixes that no input may resolve under and
  narrow read-only exceptions.
- **Ingest** hashes every input (decoded bytes for `.zst`), archives each file
  once by digest and runs the adapter. JSONL lines are stored as skeletons:
  raw bytes plus references to blobs for long string values, with the escaping
  style that reproduces each literal exactly. Work is committed in batches
  after the pack bytes are synced; an interrupted run resumes, and a source
  whose inputs and adapter version are unchanged is skipped.
- **Adapters**: `pangram` (bulk pages, annotation lines, per-item results;
  keys `bulk:<job>:<index>` and `task:<id>`), `frontier_wave` (collector
  requests, attempts, receipts, withdrawals, refusals, earlier hosted calls and
  captures), `frontier_export`, `local_journal` (paired llama-server intents and
  responses, planned requests), `local_records`, `predictions` (our detectors'
  per-document outputs), `generic` (field-mapped JSONL with optional frozen
  views and split roles) and `archive` (bytes only).
- **Link** fills derived joins: generation events to the documents they
  produced, parent text digests, and prediction rows to texts by record id.
- **Verify** checks frozen views (line and member counts, stored texts,
  declared `SHA256SUMS`/`BUILD.json`/explicit digests, and with
  `--rebuild-views` a byte-identical rebuild), observation counts and the key
  set of a Pangram observation index, source-group and text leaks between
  Train-side and evaluation roles per experiment, non-commercial licences and
  excluded collections, and store integrity.

- **Export** writes checksummed zstd JSONL tables and a `MANIFEST.json`
  (`slop_ninja_corpus_export_v1`: rows, decoded bytes and SHA-256 per file).
  `--subset all` is the whole private store: `texts` (exact text, counts,
  token count, canonical split), `records`, `events`, `observations`, `views`,
  `view_members`, `split_members`, `exposure`, `sources`, `source_inputs`,
  `files`, `file_paths`, `problems` and `text_split_roles`. `--subset
  redistributable` writes only texts already published in `datasets/`
  snapshots, with their records, views and split membership, and no
  observations or events. A source counts as published only when its root lies
  under `<repo_root>/datasets/` (an anchored prefix of the canonical repository
  path, not a substring), so a private root containing `/datasets/` is refused.
  Exact source files are rebuilt with `rebuild`.
- **`--subset public-candidate-v1`** opens the store read-only and writes a
  release candidate (`slop_ninja_public_candidate_v1`) chosen by records, split
  roles, reservations and observations under a rules file
  (`slop_ninja_public_candidate_rules_v1`). A text is released when it has a
  non-private provenance record (P), is outside every pinned reservation file
  and its source groups (R), is not sealed or unconsumed Test (S), was used by
  an experiment view, a published snapshot or a Pangram observation (U), is not
  a withdrawn generation (A), and its rights allow redistribution (L); every
  ancestor through `parent_text_sha256` must pass P, R, S, U and L as well.
  Licence and basis come from the populated record fields; attribution, licence
  URL, share-alike, notes and `redistribute_text` are rebuilt from the archived
  source line and checked against its digest. The `rights` and `exposure`
  tables are not read (no ingester writes them). Published status needs a listed
  source id rooted exactly at `<repo_root>/datasets/<name>`; any other source root
  must lie under a listed corpus root, or its texts are refused. Output:
  `texts`, `records` (allowlisted provenance), `rights` (licence, attribution,
  ancestor chain, generated-additions licence), `splits`, `experiments`,
  `pangram_observations` (rights-only exclusions keyed by digest without text),
  `hosted_generations` and `open_generations` (prompts published only when the
  digest matches, the template is allowlisted and the parent passage is embedded),
  `voices`, `generators.json`, `exclusions.json` (counts only), `LICENSES/`
  (pinned files with digests), `README.md`, `MANIFEST.json` and `SHA256SUMS`.
  Rows are built from allowlists, then a denylist gate scans every finished row
  for host paths, provider and credential identifiers, cost and route fields and
  operator values read from private files; any hit fails the export and its
  location (never the value) goes to a log beside the output directory.

Detector observations are a separate table and are never written to labels.
Build and test with one Cargo job (`cargo test --release -j 4`).
