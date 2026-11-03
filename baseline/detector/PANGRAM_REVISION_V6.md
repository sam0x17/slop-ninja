# Fresh v6 Pangram comparison

Declared on 2026-09-13 UTC while v6 fitting is active. The first epoch's
Development metrics have been observed; no v6 Test predictions or Pangram
observations on these sources have been opened. This comparison does not
change the [frozen v6 recipe](ENCODER_REVISION_V6.md) or its automatic
[evaluation handoff](ENCODER_REVISION_V6_EXECUTION.md).

Use the intersection of the complete primary and Phi-4 Test families. Retain
each selected family's human root and both routes' original draft, direct
human edit, first revision and second revision. Merge the shared human record
only when its complete record agrees between routes. Each family contributes
nine origin records. Keep every ancestor and its generator attribution.

Stratify by human-source collection and the primary route's original composer
revision, yielding nine cells. The human source and generation workflow are
the sampling units; `model_id` aliases such as `default_model` do not identify
distinct composers. Use the recorded immutable model revision instead.

Within each cell, rank families by ascending SHA256 of the JSON pair
`["slop-ninja-v6-pangram-confirmation-v1", source_group]`, with source-group
identity breaking a hash tie. Select complete rounds, one additional family
from every cell per round. Stop before the next complete round would exceed
600 billable units, or a cell is exhausted. Do not skip an expensive family
and continue farther down its cell. Count each distinct exact text hash once,
at one unit per started 100 whitespace-counted words. All cells receive the
same number of families. Retain unselected ranks and the stopping reason.

The 600-unit ceiling bounds the comparison's size; its spend is recorded in
the private ledger. Preparation makes no paid reservation or API call. Before
submission, recheck pricing, the prepared payload and the ledger.
Provider word counting and billed charges can differ from these estimates.

The selector reads frozen source records and route views, never detector
scores. Check every fresh route record against the frozen v6 historical ledger
(852 source families and 6,507 exact text hashes), plus the supplied historical
corpora. Reject source-family or exact-text overlap. The earlier source review
also checked canonical work identity and lexical-normalized text before
generation. Preserve source licenses, external-evaluation permission and
the independently recorded origin labels. Publish counts by collection,
original composer and each route's initial profile; do not rebalance after
seeing the profile counts or detector results.

After the selected v6 artifact and operating points are frozen, submit one
Pangram 4 observation per unique text using the existing bulk client and ledger.
Keep failed requests and normalized echoes visible. Do not automatically
resubmit ambiguous requests or replace texts based on their scores. Retain
all returned fractions and the provider version. These Test observations stay
outside the training corpus.

Reuse v5/v6 predictions from the declared full Test runs, joined by exact record
identity and text hash. Compare each route's R0, R1 and R2 views on this same
source intersection. For each view, use its human, selected model stage and
paired human edit. Shared humans and edits are repeated observations of the
same texts, not independent samples. For aggregate unique-text diagnostics,
take human predictions from the primary R2 report; preserve other identical-ID
exports for consistency checks. No additional model inference is needed.

Use the established Pangram flag rule, AI plus AI-assisted content fraction
at least 10%, and each detector's own frozen operating points. Report human
false positives, model-only sensitivity, mixed-origin diagnostics, paired
disagreements and provider fraction bins. Preserve the existing 512 paired
source-family bootstrap for v6-minus-v5 binary log loss and Brier score on
the selected families. No score filtering or candidate switching is permitted.

This is a budgeted comparison on families where both generation routes
completed. It does not replace the full v6 Test evaluation or the independent
117-human-root diagnostic. Its limited family count cannot establish rare
population false-positive rates or matched-FPR superiority. Pangram content
fractions and our document-origin probabilities measure different quantities.
Raw annotations remain local under the existing provider-output reuse policy.

On 2026-10-31 the budget amounts in this protocol moved to the private ledger.
The [preparation record](results/pangram-revision-v6-preparation.json) binds the
earlier bytes of this file (`protocol_sha256`
`a25abe5373dd950e11a9c10679946e436c11a5c29e1df9d6a4469f106f250d39`), which are
retained privately; `select_revision_anchor --protocol` needs those bytes to
reproduce the preparation.
