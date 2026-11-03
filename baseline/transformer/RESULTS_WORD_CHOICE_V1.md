# Word-choice pools and deeper edit combinations

This development run adds dictionary-generated word alternatives to B's grammar
menus. An assistant selected alternatives by ID after checking their meanings
in context. Rust applied the changes and measured complete-document candidates.
No model generated replacement passages for the dictionary experiment.

The source is one private development document, with 47 preservation checks,
six independent reference works and an explicit audience/voice brief. This is
an adaptive experiment, not held-out performance or a public benchmark. Text,
profiles, exact plans and observations remain in ignored private storage.

## Deeper combinations of existing edits

A nine-choice menu combined earlier word and clause proposals, editorial
repairs and an automatic grammar operation. Enumerating all subsets produced
384 compatible texts and 128 rejected combinations of overlapping edits.
Accelerator screening covered all 384 texts. The reference CPU runner confirmed
two prespecified finalists and one explicitly post hoc follow-up:

| Complete-document candidate | Reference A | Measured author distances |
| --- | ---: | --- |
| Original source | 90.39% | Comparison point |
| Lowest exploratory A, confirmed on CPU | 76.74% | Both increased |
| Lowest A among combinations with automatic fronting | 79.88% | Both increased |
| Lowest A with neither distance increased, post hoc | 83.84% | Both slightly decreased |

A here means the sum of model-only and mixed human/model class probabilities,
not a percentage of generated words. The distance constraint uses the existing
full word/grammar and function-word/grammar spaces. Neither distance establishes
author identity, intended voice or writing quality. The closest candidate by
those constraints is a useful control, without an author-fit validation claim.

The three CPU confirmations differed from their accelerator observations by at
most `2.561e-6` in combined probability. All remained above the 10% target. The
accelerator results retain their noncanonical status.

## Dictionary nomination and selection

The first pool generated 2,704 alternatives at 430 word positions and retained
512 under its declared cap. A subsequent focus on 11 positions retained all
58 alternatives. The selector chose eight contextual hypotheses before reading
their detector scores. The original source, brief, checks and reference bindings
were retained exactly. All 256 combinations were materialized and checked
against a separate reconstruction from their byte edits; no text outside the
chosen words changed.

The broad pool exposed wrong senses and register changes. Shared WordNet synset
membership and a favorable reference-frequency change were insufficient to
select a suitable replacement. This supports keeping meaning-aware selection
separate from nomination, and motivates a contextual lexical model. The current
pool is a reproducible dictionary control, with per-work counts and grammatical
context diagnostics; it is not that learned model.

All 256 dictionary combinations completed accelerator screening. The best A
was 89.67%, against 90.39% for the unchanged source. Requiring both reference
distances not to increase yielded 89.82%, with the two distances unchanged.
The reference CPU runner confirmed both rounded scores; the largest difference
from MPS was `7.947e-7` in combined probability.
Neither result approaches the target. This eight-choice neighborhood produced
a smaller reduction than the earlier clause combinations; it does not establish
a general limit on lexical editing.

The complete subset graph contains 1,024 measured edit transitions. Three word
choices increased A in every tested context, four decreased it in every context,
and one decreased it in 122 contexts but increased it in six. For that last
choice, conditional changes ranged from `-0.03212` to `+0.00363` percentage
points. These are exploratory MPS differences; individual transitions were not
confirmed on the reference runtime. Their small size and dependence on the
other choices argue for measuring edits in context. They are not fixed AI
labels attached to words or evidence of a large threshold jump.

Unigram frequency gains did not substitute for detector measurements or the
complete grammar distance. Neutral synonyms alone also do not establish an
improvement in writing. Keep the original and review any proposed final
combination against the full brief and original argument.

See the [pool protocol](LEXICAL_POOLS.md) for pinned resources, sense evidence,
license preservation and import commands. An imported nomination remains an
unreviewed hypothesis until the ordinary editorial selection requirements pass.

## Execution and limits

Builds, spaCy parsing and inference ran on the Mac Studio. Rust formatting,
all 16 B tests, Clippy and a release build passed. Real lexical import and
materialization checks retained the exact source bindings. The grammar adapter
also materialized repeated source phrases through exact byte anchors.

Reference confirmations used A V6 artifact
`4e061df4789e546dfdd58c8005860a98fb8f200a4c7642b7491461f44f16f138`,
CPU float32/eager, without truncation. Optional MPS observations remain separate.
No Pangram credits were spent in these runs. There is no new joint detector,
independent quality, mixed-model generalization or book-level milestone.
