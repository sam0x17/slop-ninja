# Structural revisions for A coverage

The saved B failure contains clause recasts and nominalizations with large
combined class-logit effects. The earlier fixed grammatical stress set covers
contractions and sentence boundaries. This pilot checks whether a public
generator can supply useful structural combinations on permitted Train material.
It uses no private B inputs, author references, B model or detector feedback.

Use the unchanged V6 Train archive, hash
`023d6e5428236502e877bac0afe0e9e9ee4aa9d10ab38eb940ca8e915cf7502e`.
For each source collection, select the first eligible source family by SHA-256
of the JSON pair `["slop-ninja-a-structural-revisions-v1", source_group]`.
Eligible parents are recorded initial model generations of 150-450 whitespace
words, with a human-only root in the same Train family. Within that family,
use the lowest record ID among eligible drafts. Preserve the complete parent,
its human root and provenance. Report empty eligible collections; do not replace
them based on a detector score or a later quality judgment.

Make one request per selected parent to the pinned public Qwen3-30B-A3B-Instruct-
2507 MLX conversion already used for corpus generation, revision
`e9675aa3ca5f900ccef55267914466d55ab325fa`. Use temperature 0.7, seed 17 and at most
2,048 output tokens. The realized response and runtime identify the observation;
a seed is not a reproducibility guarantee. Materialize and bind all requests
before launching one dedicated loopback worker on Studio. Record its model and
runtime hashes, command, requests, responses and terminal state. No automatic
retry or replacement request follows a failure. Leave shared services alone.

Request up to three nonoverlapping local alternatives, one for each category:
clause/nominalization, subject or voice recast, and adjunct placement or clause
packaging. These are proposal categories, not implemented grammar productions
or certified equivalences. Each alternative must preserve every participant,
claim, quantity, qualification, tense, modality, reference and intended tone.
Permit the generator to report a category unavailable rather than force a
misleading edit. Require exact source substrings and JSON output. Give it the
complete model parent and no detector results or examples from B.

For every mechanically accepted response, enumerate all edit subsets, retaining
the unchanged parent. Validate exact unique source spans and disjoint patches;
reject malformed responses without silently repairing them. Read the complete
parent and every resulting state before scoring. Record semantic errors,
accessibility, tone, realized construction and reference/scope ambiguity. Do
not equate a lower A score, model rationale or parse success with a valid edit.
Read the human root separately when assessing earlier generation fidelity.

After review, score every retained state and the unedited human controls with
the frozen V6 CPU runner, including editorial failures in a separate diagnostic
slice. Preserve whole-input token counts and unsupported outcomes. Compare
single edits and combinations on both class-logit and calibrated probability
scales. Near-zero failures would justify a larger independent recipe check;
uniformly easy examples would not justify bulk collection of this recipe.

This is an adaptive development pilot on Train families. It does not measure
unseen-source accuracy, establish Pangram agreement or add data to an active
fit. Any later training snapshot needs its own admission and comparison plan.
The known B texts remain evaluation-only, and the current base/large experiments
and frozen comparisons keep their existing inputs and order. No paid detector
calls are allocated to this pilot.
