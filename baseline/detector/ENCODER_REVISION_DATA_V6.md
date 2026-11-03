# Model-only revision data for the next detector

This declares a training-data collection step after the
[v5 comparison](ENCODER_NARRATIVE_RESULTS.md). It does not freeze a new
detector fit or claim a v6 result. V5 remains the released reference candidate.

V5 reduced binary log loss and human false positives on its opened tests,
but missed two Phi-4 drafts at its stricter frozen threshold. Those drafts
used Anti-AI and Fix Slop instructions. The next collection adds repeated,
substantial revisions of entirely model-written prose. It tests whether
exposure to those production histories helps the detector generalize.
Meaning, intended tone and improvement in the prose require separate review.

## Existing Train sources and selection

Use the unchanged v5 assembly, SHA256
`1b589c754dfb6d9585b9b724105057903aef3fa66a950808730d4aa6595a5cbd`.
Its Train partition contains 471 source families and 2,925 records. A
canonical historical-source audit binds 67 prior artifacts, representing
852 works. Retire all 82 works ever assigned to Test in that union. This
is a conservative superset of opened tests; it is not a claim that all
82 were scored. The audit found no overlap with v5 Train, Development or
Calibration by work identity, source group, exact text or lexical-normalized
text. No old split changes.

For each eligible Train family, choose one original model-composed draft
and its paired direct edit of the human root. Require matching generator
revision and complete prompt-profile provenance for the draft/edit pair.
Choose the draft with the smallest SHA256 of the JSON tuple
`("slop-ninja-v6-revision-train-seed-v1", source_group, model_revision, id)`.
This selection uses no detector predictions. Preserve the human root and
paired mixed-origin edit in the seed archive. Bind the exact selected IDs,
source and text hashes, eligibility audit, selector and generator executable
before generation.

## Two recorded revision passes

1. Revise each selected model draft with the pinned Qwen3-30B-A3B
   Instruct-2507 MLX conversion using the existing `anti-ai` revision mode.
2. Revise each accepted first-pass result with the pinned Mistral Small
   3.2 24B Instruct-2506 MLX conversion using `fix-slop` revision mode.

These are the same admitted Apache-2.0 generator conversions used in v5.
Pin the installed weights, tokenizer, complete runtime and model-spec bytes
again for this collection. Use the recorded model-revision prompt without
changing its instructions, temperature 0.7, at most 1,800 generated tokens,
one request at a time and an owned loopback server. No detector feedback
enters a revision request. No Pangram calls are part of this collection.

There are at most 471 first-pass and 471 second-pass requests. Pause/resume
may reuse the exact request cache; do not retry failed requests or redraw
unsatisfactory prose. Existing completion, length and formatting admission
checks apply. Retain every request, response and failure. Advance only
accepted first-pass branches. Retain all ancestors with their original
source-family split; both new passages remain `model_only`.

For later fitting, add both new stages only for complete two-pass chains.
The existing v5 Train records remain eligible when a new branch fails.
Keep failed or incomplete branches in the archive and report their counts.
The first pass includes same-model and cross-model histories according to
the selected composer; the second always changes from Qwen to Mistral.
This collection does not isolate the causal effects of revision depth,
revision style or generator choice.

## New sources and subsequent model comparison

An independent, source-only review uses PLOS search offset 700 (350 search
rows, up to 100 admitted records), Wikinews titles beginning at E (up to
100 admitted records), and the next 200 candidates in the original frozen
CMU book-summary ordering. Resolve provenance and commercial-use terms
before admission. Preserve all exclusions. Remove matches to any of the
852 historical works and exact or normalized text duplicates. Keep the
reviewed source windows fixed even when an exclusion reduces yield.

Freeze the final source allocation, generation routes, model candidates,
objective and all evaluation views before the corresponding generation
and fitting steps. Fresh confirmation must use previously unused works;
the opened v5 tests cannot select another v5 candidate. A later experiment
must retain the complete ancestry archive while selecting exactly one
human/model/mixed trio per family for Development and Calibration. Training
may expose all admitted stages with equal total loss weight per source
family and origin. Bind every selected observation to its exact text and
full archive. Compare incumbent and candidate on the same frozen views,
including unrevised and intermediate-stage diagnostics where declared.

All text, per-record decisions, request logs and private source ledgers stay
in ignored `data/baseline-detector/revision-expansion-v6/`. Publish code,
protocols, aggregate results and permitted source attribution.
