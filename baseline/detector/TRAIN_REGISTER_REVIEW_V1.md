# Human Train register inspection

The base consistency candidates raised human error on historical summaries and
news. Five inspected Test errors include promotional descriptions, formulaic
narration and source defects. Before proposing more training examples of those
kinds, inspect what the current human training data already contains.

Use the unchanged V6 Train archive, SHA256
`023d6e5428236502e877bac0afe0e9e9ee4aa9d10ab38eb940ca8e915cf7502e`.
Validate all records and ancestry, then select human roots only. Require exactly
one root per source family. For each of the four source collections, sort by
SHA256 of the JSON tuple `(slop-ninja-human-train-register-v1, collection,
source_group, id)`, with record ID as the tie-breaker. Take the first four roots
per collection. Freeze all 16 selections and complete records before reading
their text. Selection uses no detector score, keyword or editorial judgment.

Read every selected text in full with its stored origin and extraction evidence.
Describe realized register and audience, discourse form, formulaic or promotional
language, first-person or quoted voices, and completeness or source defects.
Allow overlapping descriptions and uncertainty. These are assistant observations,
not author labels or independent gold annotations. Preserve historical-proxy
evidence as weaker than a documented human-only workflow.

Report examples of coverage that exist and questions that remain. Four samples
per collection cannot establish that an unobserved register is absent from the
whole pool, or estimate its prevalence reliably. Collection names and prompt
profiles do not substitute for realized-style annotation. Do not infer that any
observed wording caused A's errors, or compare these Train samples to Test errors
as if they were matched causal controls.

Keep the exact review packet on the Studio. Publish its hash, selected IDs,
source/text hashes, stored evidence types and the textual review. Copy no private
material. This is a read-only inspection: no relabeling, source removal, new data
admission, generation, detector call or fit. The six saved-artifact Train-fit
runs and larger-backbone pair continue with their original recipes. Interpret
the complete Train-fit comparison before choosing a training intervention.
