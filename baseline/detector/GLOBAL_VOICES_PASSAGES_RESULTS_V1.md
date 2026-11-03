# Contiguous publisher passages: first review

The [Rust passage extractor](../../src/html_passage.rs) now materializes complete
paragraph runs from the ordered HTML view. It refuses to join prose across a
quotation, caption, code region, media element or unwrapped text. Source HTML,
DOM positions and normalized passage text remain separate representations.

The [frozen nominations](results/global-voices-passage-boundaries-v1.json) cover
all eight previously reviewed articles. All eight materialized successfully:
22 paragraphs and 1,323 Unicode-whitespace words. The
[manual review](results/global-voices-passage-review-v1.json) read every complete
candidate against the prior article review.

| Work | Paragraphs | Words | Review finding |
| --- | ---: | ---: | --- |
| gv:769472 | 2 | 183 | Connected opening with an existing date contradiction, retained unchanged. |
| gv:614081 | 2 | 217 | Connected first-person opening; the later interview and change in motivation are outside the excerpt. |
| gv:594642 | 4 | 182 | Connected workshop background ending before a new heading. |
| gv:711372 | 1 | 57 | Complete short opening; little long-context coverage and no later arrest outcome. |
| gv:555064 | 4 | 199 | Connected historical and election context; allegations retain their attribution. |
| gv:662657 | 5 | 288 | Connected case opening including the temporary suspension; later procedural details remain outside it. |
| gv:557838 | 3 | 136 | Interview introduction without the promised discussion; further context review required. |
| gv:742458 | 1 | 61 | Complete short opening; little long-context coverage. |

Seven candidates have locally connected openings, with the stated limits. The
interview introduction shows why successful extraction cannot establish a useful
passage. Short candidates and the unsuitable boundary choice remain in the
ledger; no post hoc filter or replacement made the results look cleaner.

The [run report](results/global-voices-passage-run-v1.json) binds the source,
boundaries and compiled implementation. Its original `materialized_unreviewed`
status records the extraction stage; the separate review records later judgments.
The [binding check](results/global-voices-passage-check-v1.json) verified all
source hashes, exact paragraph texts, DOM ownership and contiguous boundaries.
Those checks do not prove coherence, attribution, human origin or reuse rights.

Three focused tests cover complete paragraph retention, forbidden intervening
regions and explicit boundary/recovery failures. Formatting, all-target tests,
Clippy with warnings denied, the build and the eight-work run passed on the
Studio. Linux execution remains unverified. No heavy work ran on the laptop.

This is source preparation under the [declared protocol](GLOBAL_VOICES_PASSAGES_V1.md).
It adds no English productions, training records, detector observations or
Pangram calls. Exact source and candidate texts stay outside Git. All eight
works have informed development and supply no unseen-source accuracy estimate.
