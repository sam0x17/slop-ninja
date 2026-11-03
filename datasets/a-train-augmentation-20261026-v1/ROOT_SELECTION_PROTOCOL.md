# Train augmentation root selection v1

Initially frozen before the root-length/text-duplicate scan. This selects original V6 Train
human roots for a cumulative dataset expansion; it does not alter any existing
dataset, split, model, selection or protocol. Original V6 Dev/CAL/Test and all
72 fresh-cohort works and reference works remain protected. No Test prose is read.

Requested selection: 96 independent works, at most 24 from each exact collection
`cmu-books-historical`, `hc3-wiki-historical`, `plos_one`, `wikinews`.
Order each collection by ascending SHA-256 of UTF-8
`slop_ninja_train_augmentation_roots_v1` followed by one NUL byte followed by
`source_group`. Break a hash tie by source_group, then original record ID.
One original historical human root per source family. No detector/model scores,
style judgments or generated outputs influence selection. A collection shortfall
is retained and reported; do not fill it from another collection or relax bounds.

Require original split Train, origin human_only, evidence historical_proxy,
no parent or generation, exact source/text hashes, nonempty source provenance,
and explicit commercial_training, model_release, external_evaluation and
redistribute_text rights. Retain all original attribution, licensing, notices and
history. Original publication or historical-revision metadata must precede
2022-11-01. Prefer source.published_at; when absent, accept the exact
`Historical revision timestamp: YYYY-MM-DD...` field in the original pinned
evidence_notes. Never infer dates from a title or a model judgment. The historical
evidence is a usable proxy, not documented contemporary human-only production.
Keep its zero-share endpoint with that limitation; unknown individual writer
identity remains null, including collective Wikipedia authors and coauthored PLOS.

Use each entire original record unchanged if it contains 150 through 450 Unicode
whitespace-delimited words, inclusive. No cropping, repair, whitespace changes or
new text. This word range is not tokenizer qualification; later preprocessing
must check V6's 2,048-token bound without truncation.

Before admitting candidate prose, check the 72-work TSV metadata against source
families, normalized source titles and available author identities. Title keys
lowercase Unicode, replace punctuation with spaces, discard leading articles,
and include the main-title variant before a colon/semicolon or parenthesis.
Known withheld author names/keys and all explicit work aliases are blocked where
metadata identifies them. Missing individual authorship does not blanket-exclude
historical Wikipedia prose or invent the summarized book author's authorship.
Record the limit that alias/translation and subject-book author metadata can be
incomplete. Also block prior protected source families available in the original
metadata-only V6 source credits, and all current dataset/reference families.

Compare exact and whitespace-normalized text SHA-256 against the existing 929-row
non-Test dataset and separate eight non-Test references, after checking their
manifest hashes, splits and purposes. No Test inputs or reserved Test prose paths
are opened. Reject duplicate candidate text/families, retaining the first in the
fixed hash order. Preserve existing source-family boundaries and immutable files.

Emit supervised humans with frozen_split=train, purpose=query,
human_identity=null, human_role_present=true, ai_identity=null,
ai_role_present=false, documented_human_only=false, share_target=0,
grammar_exposure=unknown, and metadata.source_collection set to the exact original
collection. Each row keeps original text, SHA-256, original record history, input
file/line hashes, whole-record byte span and rights. The manifest binds the
protocol, implementation, input metadata/text files, selection and exclusion
audit. A new output directory is required; existing outputs are never replaced.

The subsequent parent-owned recipe uses the already pinned current Qwen3.6 and
Phi identities, with scratch plus 10/40/80/100 percent rewrite tasks. Requested
fraction differs from realized final-word share; preserve exact source/replacement
spans and failures. No generation occurs during selection. Historical Qwen3-30B
remains distinct from current Qwen3.6. No new Train Mistral/Gemma ancestry is
admitted; frozen V6's prior Mistral exposure cannot be undone by this restriction.

Metadata-field correction: the initial protocol and 72-root output remain at
ROOT_SELECTION_PROTOCOL.initial.md and roots-initial-date-field-v1/. The first
implementation checked only source.published_at and omitted all 53 Wikinews roots
despite their retained historical-revision dates. This correction was recorded
before the corrected selection scan. It changes the accepted metadata location,
not the historical cutoff, length range, ranking, quotas or blacklist. No model
or detector result informed it.
