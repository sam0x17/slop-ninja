# Narrative generation: editing-strength snapshot

The user subsequently clarified that fully model-written, strongly style-steered
prose is the main target. The [v5 amendment](ENCODER_NARRATIVE_V5_TARGET.md) applies
that priority to selection and evaluation. The measurements below describe the
auxiliary edit examples; lack of light assistance is not a main-target gap.

Measured during the running v5 Qwen campaign, before any v5 fitting or detector
scores. This is a snapshot of completed Train outputs, including successful
siblings of families that may later fail complete-pair admission. It is not the
final cohort. The [aggregate report](results/narrative-edit-audit-snapshot-v1.json)
binds the source corpus, run, audit executable and detailed local observation.

The snapshot contained 167 completed record files: 112 Train outputs and 55
held-out outputs excluded from overlap analysis. The Train outputs comprised
63 drafts and 49 edits.

| Measurement | Drafts | Edits |
| --- | ---: | ---: |
| Median source words retained in order | 43.1% | 53.6% |
| Mean source words retained in order | 43.6% | 54.2% |
| Outputs retaining less than half the source words | 48/63 | 17/49 |
| Outputs retaining at least 95% of source words | 0/63 | 0/49 |
| Identical after whitespace collapse | 0/63 | 0/49 |
| Identical normalized word sequence | 0/63 | 0/49 |

`audit_edits` computes the longest common subsequence of lowercased Unicode word
tokens, divided by the number of source tokens. Repeated words must match in
order. A separate contiguous-run measurement distinguishes uninterrupted copied
passages from scattered overlap. The command also measures how much of the
output comes from the source and checks identity before and after whitespace
normalization. It makes no model calls and changes no text, labels or admission
decisions. One numeric unit test covers repeated words, reordered words,
contiguous runs and disjoint sequences; the required Rust checks passed.

The edits show substantial surface rewriting despite the light-to-moderate
editing instruction. None in this snapshot was a whitespace-only copy, but none
reached 95% ordered word retention either. The resulting mixed-origin training
examples supply limited evidence about light assistance. That limitation must
remain explicit when interpreting the detector's mixed-class results.

Overlap cannot establish preserved meaning. The existing
[generation preflight](GENERATION_QA.md) already found factual changes in earlier
source-conditioned drafts and edits. This snapshot makes no new fidelity claim.
It changes neither the frozen v5 generation protocol nor the Test selection rule.
Repeat the audit after each full cohort finishes, retaining this observation
separately rather than replacing it with final-cohort counts.
