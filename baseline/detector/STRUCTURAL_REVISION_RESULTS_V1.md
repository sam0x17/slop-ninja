# Structural revision pilot: one larger A failure

One accepted revision lowered frozen V6 from 90.94% to 72.84%, crossing its
stricter detection threshold. It combines a passive recast with replacing a
repeated name by a pronoun; this result does not separate their effects. The
other accepted states remained above 97%. No candidate reached 10%, and no
Pangram calls were made. V6 remains the reference detector.

The [pilot](STRUCTURAL_REVISION_PILOT_V1.md) selected one existing Train family
per collection before generating proposals. There were 326 eligible drafts in
the book-summary collection, 236 encyclopedia drafts, 78 scientific drafts and
103 news drafts. The selected parents contained 186-240 whitespace words.
Their generators were Qwen for the book summary and abstract, and OLMo for the
encyclopedia entry and news report. All four have recorded commercial-training
and model-release permissions in the unchanged Train archive.

One pinned public Qwen request per parent produced twelve proposed alternatives.
The encyclopedia response used undeclared category names and overlapping spans;
it was rejected without repair or replacement. The other three responses each
produced eight states, including the unchanged parent. The complete four
source pairs, four responses and 24 materialized states were read before new
detector scores were opened.

Of the 21 edited states, four were accepted relative to their immediate model
parent, fourteen had semantic failures and three had unresolved reference or
prose concerns. The four accepted states come from two source families: three
scientific variants, including their combination, and one news variant. These
are dependent observations. All four earlier model parents also differed
substantively from their human roots; acceptance of a new edit does not approve
those earlier errors.

| Collection | Parent A | Lowest A, all materialized states | Lowest A, accepted edits | Strict flags, model states |
| --- | ---: | ---: | ---: | ---: |
| Book summary | 99.24% | 99.18% | None accepted | 8/8 |
| Encyclopedia | 99.18% | Parent only | None materialized | 1/1 |
| Scientific abstract | 97.42% | 96.82% | 97.25% | 8/8 |
| News | 90.94% | 72.84% | 72.84% | 6/8 |

Scores are model-or-assistance probabilities. The existing strict cutoff is
greater than 84.3649%; the looser cutoff is greater than 42.8737%. All 25 model
states exceeded the looser cutoff; 23 exceeded the strict one. Four unedited
human controls scored 0.16-1.78% and none was flagged. These are Train controls,
so this is not a held-out accuracy or false-positive estimate.

The news revision changed the recovered model-human logit difference from 2.01
to -0.44 and the mixed-human difference from 3.69 to 1.20. Its 18.10-point drop
is larger than the 0.31-point maximum model decrease in the earlier fixed
contraction/boundary diagnostic. The cohorts and interventions differ; this is
an observed failure to investigate, not a controlled comparison of rule families.
Adding the other proposed news edits raised A again and introduced quality
problems. The pilot did not reproduce the earlier three-edit near-zero collapse.

## Grammar and preservation findings

The manual review identified requirements for reusable edit rules:

- Preserve every argument in a voice change, including reflexive recipients.
  One proposal lost the fact that the subject was teaching himself.
- Keep locations and materials distinct from agents. One proposal made the
  specimens perform the detection previously reported within them.
- Recheck the complete sentence after changing its subject. A nominalization
  left an introductory role adjunct dangling because the new subject denoted
  an abstract role rather than its holder.
- Preserve the scope of supplementary relatives and location references.
  The model's construction labels and explanations sometimes misidentified
  the syntax or claimed clarity despite new attachment ambiguity.
- Do not fill a nominalized sentence with an unsupported motivational or causal
  predicate merely to complete its grammar.

These are reviewed transformation failures, not measured native-parser errors
or newly implemented productions. They support using grammar constraints to
limit proposals and inspecting each complete combination before acceptance.
The subsequent [component diagnostic](STRUCTURAL_COMPONENTS_RESULTS_V1.md)
separated the successful news revision's voice and pronoun changes. The passive
alone lowered A to 67.71%, while the pronoun alone slightly raised it. A later
[three-source follow-up](VOICE_PROBES_RESULTS_V1.md) found drops below 0.35 points;
it did not reproduce the large news effect.

## Evidence

The [edit review](results/structural-revisions-review-v1.json) records every
accepted, failed and unresolved mask. The [parent review](results/structural-revisions-parent-review-v1.json)
keeps earlier source-fidelity errors separate. The
[complete V6 report](results/structural-revisions-v6-v1.json) includes all 29
inputs, class probabilities, token counts, flags and class-logit interactions.
No input was truncated or unsupported; the maximum was 419 tokens.

The Rust preparer, capture client and materializer passed Studio formatting,
all-target tests and Clippy with warnings denied. Independent byte-based patch
reconstruction matched every materialized state. The dedicated generation run
checked 195 bound files before and after its four requests and closed normally.
CPU scoring used the unchanged bundled V6 runner with one thread. Exact input
coverage, response identity, calibration, threshold arithmetic and reconstruction
of class contrasts from subset coefficients were checked. An
[independent arithmetic check](results/structural-revisions-check-v1.json)
matched all 29 rows and the three complete subset transforms within `1e-10`.
Full text, generator
responses and local execution files remain in the ignored Studio archive.

This result admits no data to a fit and supplies no new evidence of Pangram
agreement. The current origin-consistency and capacity comparisons retain their
frozen inputs, thresholds and execution order.
