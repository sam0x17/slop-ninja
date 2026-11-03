# Complete-document coverage and contextual A scoring

This research path prepares exact plain text for A without truncation. It records
every scoring context and any missing evidence. It does not yet edit a book,
verify cross-chapter meaning, or provide a calibrated whole-book detector.

The Rust `document` command handles source ranges, coverage, score validation and
reuse. A small Python bridge loads the canonical artifact's tokenizer, verifies
its content manifest and tokenizer assets, and checks the tokenizer package
versions. It never loads detector weights. Execute both the bridge and the
separate canonical A runner on the Studio.

## Frozen context policy

`exact_utf8_whitespace_snap_768_stride_two_views_v1` uses these rules:

1. Preserve the complete UTF-8 source, including all whitespace. Tokenize it
   without truncation and convert character offsets to source byte positions.
   Never reconstruct text by decoding tokens.
2. Place primary cuts every 768 content tokens. Place shifted cuts at token 384,
   then every 768 tokens. Snap each cut to the beginning of its whitespace-delimited
   word; retain the original whitespace around cuts. Include source start and end
   in both views. Deduplicate coincident cuts.
3. Each window spans two consecutive cut intervals. At either end it may be
   shorter; when a view has only one interval, use the complete source. Thus each
   view covers all source bytes, with overlap between neighboring windows. Ordinary
   interior windows contain about 1,536 tokens. Placement never depends on scores.
4. Count every distinct window again with the canonical tokenizer, including
   special tokens. The artifact's actual input limit applies. Snapping, Unicode,
   or a pathological long word can make a window unsupported. Retain that window
   and its reason; do not silently shorten it or omit the uncovered evidence.
5. Include the complete source as a `whole` view. Export a direct A request when
   it fits the artifact limit. Otherwise record its unsupported length. A caller
   may add predeclared byte-range probes for paragraphs or section boundaries.
   Probes do not replace the two complete coverage views.

Requests are deduplicated by exact text SHA-256. The plan binds the source bytes,
every range and token count, artifact manifest, tokenizer bridge and execution
policy. Raw source plans and responses remain in ignored storage; private text
and its identifying manifests belong in `data/grammar-private/`.

## Commands

Run on the Studio with the pinned detector environment:

```sh
slop_ninja_transformer document prepare \
  --source /private/work/source.txt \
  --artifact /models/reference-a \
  --python /environments/detector/bin/python \
  --out /private/work/source-plan

HF_HUB_OFFLINE=1 TRANSFORMERS_OFFLINE=1 \
  /environments/detector/bin/python /models/reference-a/runner/infer.py \
  --artifact /models/reference-a \
  < /private/work/source-plan/detector-requests.jsonl \
  > /private/work/source-scores.jsonl

slop_ninja_transformer document report \
  --plan /private/work/source-plan/plan.json \
  --scores /private/work/source-scores.jsonl \
  --out /private/work/source-report.json
```

Optional `--probes /private/work/probes.json` accepts an array of
`{"label":"section-boundary","start":100,"end":900}` objects. Offsets are
UTF-8 byte indices into the exact source. Invalid, empty and reversed ranges fail.
Freeze the probe policy before inspecting scores, and retain all results.

The report checks artifact identity, label order, execution runtime, token counts
and probability consistency. It rejects unknown or duplicate observations.
Missing and unsupported responses remain visible, with unscored byte ranges in
each complete view. Overlap can cover every byte while one requested context is
still missing, so the report separately records whether all windows were scored.
Each window keeps its full response. Minima, maxima and the all-windows threshold
diagnostic are reported separately for the two views; no window mean is presented
as a document probability. A direct supported `whole` observation remains distinct.

## Rescoring after edits

Prepare the revised complete source using the same policy. Then reuse previous
observations and export the remaining requests:

```sh
slop_ninja_transformer document reuse \
  --plan /private/work/revision-plan/plan.json \
  --prior-plan /private/work/source-plan/plan.json \
  --prior-scores /private/work/source-scores.jsonl \
  --out /private/work/revision-reuse
```

Score `revision-reuse/detector-requests.jsonl` using the same canonical runner.
Pass both `--scores revision-reuse/cached-scores.jsonl` and `--scores new-scores.jsonl`
to `document report`. Each successful reuse requires the complete scoring text to
match, along with the artifact, tokenizer and execution policy. An unchanged
sentence in changed surrounding text needs a new score. Failed observations are
never treated as successful cached evidence. Reuse manifests retain the previous
plan and response-file hashes. These are research provenance records, not signed
execution proofs for subnet settlement.

Token insertions can shift later cuts and invalidate many windows. Version 1
prefers conservative rescoring over stale evidence. Stable section identities,
incremental parsing and budgeted search remain needed for efficient book editing.
There is no full-book edit-distance calculation or spaCy parse in this coverage
path. Its memory and tokenization work grow with document length and the fixed
number of overlapping views.

## Authorized Pangram context comparisons

The detector crate's `pangram_bulk plan-b-context` freezes explicitly authorized
`whole` and named `probe:` views from a document plan. This supports private
manuscript diagnostics separately from reviewed synthetic B selections and
training-corpus annotations. It is not an editorial acceptance route.

```sh
pangram_bulk plan-b-context \
  --document-plan /private/work/source-plan/plan.json \
  --view whole --view probe:closing-section \
  --authorization-note 'User requested Pangram checks of these exact views' \
  --output-dir /private/work/pangram-context-plan
```

The note records authorization already given by the user. Planning does not
contact Pangram. Validation binds the complete source, selected byte ranges,
request payload and authorization record. Submission uses the existing cumulative
ledger, reservations and no-retry policy. The recorded views retain distinct
roles even when identical submitted bytes permit deduplication. No reference
samples are submitted, no public dashboard link is requested, and no training
admission follows from a low score. Keep the plan, inputs, responses and
identifying manifests in ignored private storage.

The bulk endpoint documents a 1,000-unit request limit, with one Pangram 4 unit
per started 100-word block per item. This planner checks that limit; normal HTTP
body limits and provider acceptance still apply. It does not impose the older
synthetic experiment's 1,000-word limit or silently split an authorized document.
[Pangram Bulk API](https://docs.pangram.com/api-reference/bulk-api)

Collection uses the separate `slop_ninja_b_private_context_pangram_annotation_v1`
schema. Preserve all returned fractions and windows. Compare exact submitted
texts and record any allowed whitespace normalization. A context comparison
uses different amounts of the same text; it is not an input/output rewrite pair.

## Evidence limits

A successful coverage run establishes that the wrapper accounts for the text.
It does not establish detector accuracy, meaningful stylistic improvement, or
semantic preservation. Complete-work benchmarks and independent editorial review
remain required. Repeating a short synthetic passage to test book-sized input
is an engineering check only. No windowed Pangram substitute, attestation,
document-format adapter or trained B model is added by this protocol. Provider
calls require an explicitly authorized, budgeted submission after preparation.
